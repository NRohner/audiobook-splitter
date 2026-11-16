// Chapter-break detection: silences become candidates, Whisper transcribes the
// speech either side, and the transcripts are matched against the EPUB chapters.
use crate::book::Chapter;
use crate::matching::{align, bigram_score, normalize};
use crate::transcribe::{clip_samples, Transcriber};
use crate::Silence;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

// ponytail: fixed clip length (spec said 15-30s); make it a setting if tuning needs it.
pub const CLIP_SECONDS: f64 = 20.0;
// Silences this close to a file's start/end are covered by the file boundary itself.
const EDGE_SECONDS: f64 = 1.0;

pub struct AudioFile {
    pub path: PathBuf,
    pub silences: Vec<Silence>,
    pub duration: f64,
}

// A possible chapter break: a silence, or the start of a file (start == end == 0).
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub file: usize,
    pub start: f64,
    pub end: f64,
}

impl Candidate {
    pub fn split_at(&self) -> f64 {
        (self.start + self.end) / 2.0
    }
}

#[derive(Debug, Clone)]
pub struct ChapterBreak {
    pub file: usize,
    pub at: f64,
    pub chapter: usize,
    pub score: f64,
    pub before: String,
    pub after: String,
}

#[derive(Debug)]
pub struct ChapterMatch {
    pub breaks: Vec<ChapterBreak>,
    pub missing: Vec<usize>, // Chapters with no matching break.
}

// What to write for one input file: segment end times.
#[derive(Debug, PartialEq)]
pub struct SplitJob {
    pub path: PathBuf,
    pub points: Vec<f64>,
}

// One output chapter: the stretches of input files it plays, in order.
#[derive(Debug, PartialEq)]
pub struct Segment {
    pub name: String,
    pub pieces: Vec<Piece>,
}

#[derive(Debug, PartialEq)]
pub struct Piece {
    pub path: PathBuf,
    pub start: f64,
    pub end: f64,
}

// File starts plus every long-enough silence, in playback order.
pub fn candidates(files: &[AudioFile], min_silence: f64) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (i, f) in files.iter().enumerate() {
        out.push(Candidate { file: i, start: 0.0, end: 0.0 });
        out.extend(
            f.silences
                .iter()
                .filter(|s| s.duration >= min_silence && s.start >= EDGE_SECONDS && s.end <= f.duration - EDGE_SECONDS)
                .map(|s| Candidate { file: i, start: s.start, end: s.end }),
        );
    }
    out
}

// Transcribes the speech before and after each candidate. Returns (before, after) text.
// Workers decode clips with FFmpeg while others hold the transcriber's states, so
// Whisper rarely waits on decoding; each worker holds at most one candidate's clips.
pub fn transcribe_candidates(
    files: &[AudioFile],
    cands: &[Candidate],
    transcriber: &Transcriber,
    mut progress: impl FnMut(usize, usize),
) -> Result<Vec<(String, String)>, String> {
    let clip = |file: &AudioFile, from: f64, to: f64| -> Result<String, String> {
        if to - from < 0.5 {
            return Ok(String::new());
        }
        transcriber.transcribe(&clip_samples(&file.path, from, to - from)?)
    };
    let next = AtomicUsize::new(0);
    thread::scope(|s| {
        let (tx, rx) = mpsc::channel();
        // ponytail: two decoders per state; tune if benchmarks show Whisper still waiting.
        for _ in 0..transcriber.states() + 2 {
            let (tx, clip, next) = (tx.clone(), &clip, &next);
            s.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(c) = cands.get(i) else { break };
                let file = &files[c.file];
                let pair = clip(file, (c.start - CLIP_SECONDS).max(0.0), c.start)
                    .and_then(|before| Ok((before, clip(file, c.end, (c.end + CLIP_SECONDS).min(file.duration))?)));
                if tx.send((i, pair)).is_err() {
                    break; // Another candidate failed.
                }
            });
        }
        drop(tx);
        let mut out = vec![(String::new(), String::new()); cands.len()];
        for (done, (i, pair)) in rx.iter().enumerate() {
            out[i] = pair?; // Returning drops `rx`, which stops the workers.
            progress(done + 1, cands.len());
        }
        Ok(out)
    })
}

// The part of a chapter a clip of `n` words should line up with: barely more than
// the clip itself, so a pause partway into a chapter doesn't match its opening.
fn edge_len(n: usize) -> usize {
    n + n / 5 + 10
}

// Picks one candidate per chapter (in order). A candidate qualifies when the speech
// after it matches the chapter's opening at `threshold` or better; a matching
// ending of the previous chapter before it adds to the score.
pub fn pick_breaks(
    cands: &[Candidate],
    transcripts: &[(String, String)],
    chapters: &[Chapter],
    threshold: f64,
) -> ChapterMatch {
    let words = normalized(transcripts);
    let score = |ch: usize, c: usize| match_score(chapters, ch, &words[c], threshold);

    let assigned = align(chapters.len(), cands.len(), &score);
    let mut breaks = Vec::new();
    let mut missing = Vec::new();
    for (ch, c) in assigned.into_iter().enumerate() {
        match c {
            Some(c) => breaks.push(chapter_break(&cands[c], &transcripts[c], ch, score(ch, c).unwrap_or(0.0))),
            None => missing.push(ch),
        }
    }
    ChapterMatch { breaks, missing }
}

fn normalized(transcripts: &[(String, String)]) -> Vec<(Vec<String>, Vec<String>)> {
    transcripts.iter().map(|(b, a)| (normalize(b), normalize(a))).collect()
}

// How well the words heard either side of a candidate fit the start of chapter `ch`;
// None when the speech after it doesn't match the chapter's opening at `threshold`.
fn match_score(chapters: &[Chapter], ch: usize, (before, after): &(Vec<String>, Vec<String>), threshold: f64) -> Option<f64> {
    let head = &chapters[ch].words[..edge_len(after.len()).min(chapters[ch].words.len())];
    let after_score = bigram_score(after, head);
    if after_score < threshold {
        return None;
    }
    let before_score = ch.checked_sub(1).map_or(0.0, |prev| {
        let w = &chapters[prev].words;
        bigram_score(before, &w[w.len().saturating_sub(edge_len(before.len()))..])
    });
    Some(after_score + before_score)
}

fn chapter_break(c: &Candidate, (before, after): &(String, String), chapter: usize, score: f64) -> ChapterBreak {
    ChapterBreak { file: c.file, at: c.split_at(), chapter, score, before: before.trim().to_string(), after: after.trim().to_string() }
}

// A position in the book: (file index, seconds into that file).
pub type Pos = (usize, f64);

// Where to look again for a missing chapter: from the nearest found chapter before it
// to the nearest found after it, or the start/end of the book.
pub fn rescan_window(found: &[ChapterBreak], ch: usize, files: &[AudioFile]) -> (Pos, Pos) {
    let before = found.iter().filter(|b| b.chapter < ch).max_by_key(|b| b.chapter);
    let after = found.iter().filter(|b| b.chapter > ch).min_by_key(|b| b.chapter);
    let end = files.len().saturating_sub(1);
    (
        before.map_or((0, 0.0), |b| (b.file, b.at)),
        after.map_or((end, files.get(end).map_or(0.0, |f| f.duration)), |b| (b.file, b.at)),
    )
}

// The window as (file, from, to) ranges, one per file it touches.
pub fn window_ranges(files: &[AudioFile], from: Pos, to: Pos) -> Vec<(usize, f64, f64)> {
    (from.0..=to.0)
        .map(|f| (f, if f == from.0 { from.1 } else { 0.0 }, if f == to.0 { to.1 } else { files[f].duration }))
        .filter(|&(_, start, end)| end - start > 0.01)
        .collect()
}

// Candidates strictly inside the window; its ends are the breaks it runs between.
pub fn window_candidates(files: &[AudioFile], from: Pos, to: Pos, min_silence: f64) -> Vec<Candidate> {
    candidates(files, min_silence)
        .into_iter()
        .filter(|c| (c.file, c.split_at()) > (from.0, from.1 + EDGE_SECONDS) && (c.file, c.split_at()) < (to.0, to.1 - EDGE_SECONDS))
        .collect()
}

// The candidate that best starts chapter `ch`, if any matches at `threshold`.
pub fn rescan_chapter(
    cands: &[Candidate],
    transcripts: &[(String, String)],
    chapters: &[Chapter],
    ch: usize,
    threshold: f64,
) -> Option<ChapterBreak> {
    let words = normalized(transcripts);
    (0..cands.len())
        .filter_map(|c| Some((c, match_score(chapters, ch, &words[c], threshold)?)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(c, score)| chapter_break(&cands[c], &transcripts[c], ch, score))
}

// Where a chapter starts, as edited in the GUI. `file` indexes the analyzed files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Break {
    pub file: usize,
    pub at: f64,
    pub title: String,
}

// Matched breaks as titled chapter starts, with an "Opening" for any audio before the first.
pub fn chapter_breaks(m: &ChapterMatch, chapters: &[Chapter]) -> Vec<Break> {
    let mut out: Vec<Break> = m
        .breaks
        .iter()
        .map(|b| Break {
            file: b.file,
            at: b.at,
            title: chapters[b.chapter].title.clone().unwrap_or_else(|| format!("Section {}", b.chapter + 1)),
        })
        .collect();
    if !out.first().is_some_and(|b| b.file == 0 && b.at <= 0.01) {
        out.insert(0, Break { file: 0, at: 0.0, title: "Opening".into() });
    }
    out
}

// Without an EPUB: a break at every file start and long-enough silence.
pub fn silence_breaks(files: &[AudioFile], min_silence: f64) -> Vec<Break> {
    candidates(files, min_silence)
        .iter()
        .enumerate()
        .map(|(i, c)| Break { file: c.file, at: c.split_at(), title: format!("Part {}", i + 1) })
        .collect()
}

// Turns chapter starts into chapters, each named after its title. A chapter running on
// into the next file takes in that file's opening too, so it comes out as one track.
pub fn plan_from_breaks(files: &[AudioFile], breaks: &[Break]) -> Result<Vec<Segment>, String> {
    let mut breaks = breaks.to_vec();
    for b in &mut breaks {
        let duration = files.get(b.file).ok_or("A chapter break points at an unknown file.")?.duration;
        if !(b.at >= 0.0 && b.at < duration - 0.01) {
            return Err(format!("'{}' starts outside its audio file.", b.title));
        }
        if b.at <= 0.01 {
            b.at = 0.0;
        }
    }
    breaks.sort_by(|a, b| (a.file, a.at).partial_cmp(&(b.file, b.at)).unwrap());
    if let Some(w) = breaks.windows(2).find(|w| w[0].file == w[1].file && w[1].at - w[0].at <= 0.01) {
        return Err(format!("'{}' and '{}' start at the same time.", w[0].title, w[1].title));
    }

    let mut segments = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let mut start = 0.0;
        for b in breaks.iter().filter(|b| b.file == i) {
            if b.at > 0.0 {
                add_piece(&mut segments, f, start, b.at);
            }
            segments.push(Segment { name: b.title.trim().to_string(), pieces: Vec::new() });
            start = b.at;
        }
        add_piece(&mut segments, f, start, f.duration);
    }
    Ok(segments)
}

// Adds audio to the chapter still playing, or to an "Opening" before the first chapter.
fn add_piece(segments: &mut Vec<Segment>, f: &AudioFile, start: f64, end: f64) {
    if segments.is_empty() {
        segments.push(Segment { name: "Opening".into(), pieces: Vec::new() });
    }
    segments.last_mut().unwrap().pieces.push(Piece { path: f.path.clone(), start, end });
}

// Makes a chapter title safe to use as a file name.
pub fn sanitize_file_name(name: &str) -> String {
    let name: String = name
        .trim_matches(|c: char| c.is_whitespace() || c == '.')
        .replace(':', " -")
        .chars()
        .map(|c| if c.is_control() || "/\\*?\"<>|".contains(c) { '_' } else { c })
        .take(120)
        .collect();
    // Windows drops trailing dots/spaces, so trim again after truncating.
    let name = name.trim_end_matches(|c: char| c.is_whitespace() || c == '.');
    if name.is_empty() {
        return "Untitled".to_string();
    }
    // Device names Windows won't create as files, with or without an extension.
    let upper = name.to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&upper.as_str())
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && matches!(upper.as_bytes()[3], b'1'..=b'9'));
    if reserved { format!("{}_", name) } else { name.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sil(start: f64, end: f64) -> Silence {
        Silence { start, end, duration: end - start }
    }

    fn file(name: &str, silences: Vec<Silence>, duration: f64) -> AudioFile {
        AudioFile { path: PathBuf::from(name), silences, duration }
    }

    fn chapter(title: &str, text: &str) -> Chapter {
        Chapter { title: Some(title.to_string()), words: normalize(text) }
    }

    fn c(file: usize, start: f64, end: f64) -> Candidate {
        Candidate { file, start, end }
    }

    #[test]
    fn candidates_include_file_starts_and_skip_edges_and_short_silences() {
        let files = [
            file("a.mp3", vec![sil(0.0, 2.5), sil(10.0, 13.0), sil(20.0, 20.5), sil(97.5, 100.0)], 100.0),
            file("b.mp3", vec![sil(30.0, 33.0)], 60.0),
        ];
        assert_eq!(
            candidates(&files, 2.0),
            vec![c(0, 0.0, 0.0), c(0, 10.0, 13.0), c(1, 0.0, 0.0), c(1, 30.0, 33.0)]
        );
    }

    const CH1: &str = "Chapter One. The morning fog rolled over the harbor as the fishing boats returned with their nets full of silver herring and the gulls screamed overhead. \
        Old Tom stood at the end of the pier and counted every boat twice. The last one came in late and empty.";
    const CH2: &str = "Chapter Two. Nobody in the village could remember a winter as cold as that one. The well froze solid in December \
        and the children skated across the mill pond while their parents carried water up from the river in buckets.";

    fn transcripts(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(b, a)| (b.to_string(), a.to_string())).collect()
    }

    #[test]
    fn pick_breaks_finds_chapter_starts_and_ignores_mid_chapter_silences() {
        let chapters = [
            chapter("Copyright", "All rights reserved. No part of this book may be reproduced in any form without permission from the publisher."),
            chapter("Chapter One", CH1),
            chapter("Chapter Two", CH2),
        ];
        let cands = [c(0, 0.0, 0.0), c(0, 40.0, 43.0), c(0, 90.0, 93.0)];
        let t = transcripts(&[
            ("", "Chapter 1. The morning fog rolled over the harbour as the fishing boats returned with their nets full of silver herring."),
            ("as the fishing boats returned", "Old Tom stood at the end of the pier and counted every boat twice."), // mid-chapter pause
            ("Old Tom stood at the end of the pier and counted every boat twice. The last one came in late and empty.",
             "Chapter two. Nobody in the village could remember a winter as cold as that one. The well froze solid in December."),
        ]);
        let m = pick_breaks(&cands, &t, &chapters, 0.4);
        let found: Vec<(usize, usize)> = m.breaks.iter().map(|b| (b.chapter, cands.iter().position(|x| x.split_at() == b.at).unwrap())).collect();
        assert_eq!(found, vec![(1, 0), (2, 2)]);
        assert_eq!(m.missing, vec![0]);
        assert!(m.breaks[1].score > 1.0, "before + after should both count: {}", m.breaks[1].score);
    }

    #[test]
    fn rescan_window_spans_the_nearest_found_chapters() {
        let files = [file("a.mp3", vec![], 100.0), file("b.mp3", vec![], 80.0)];
        let found = [brk(0, 50.0, 8), brk(1, 20.0, 10), brk(0, 5.0, 2)];
        assert_eq!(rescan_window(&found, 9, &files), ((0, 50.0), (1, 20.0)));
        assert_eq!(rescan_window(&found, 1, &files), ((0, 0.0), (0, 5.0))); // Before the first.
        assert_eq!(rescan_window(&found, 11, &files), ((1, 20.0), (1, 80.0))); // After the last.
    }

    #[test]
    fn window_ranges_split_a_window_by_file() {
        let files = [file("a.mp3", vec![], 100.0), file("b.mp3", vec![], 80.0), file("c.mp3", vec![], 50.0)];
        assert_eq!(window_ranges(&files, (0, 60.0), (2, 10.0)), vec![(0, 60.0, 100.0), (1, 0.0, 80.0), (2, 0.0, 10.0)]);
        assert_eq!(window_ranges(&files, (1, 5.0), (1, 30.0)), vec![(1, 5.0, 30.0)]);
        assert_eq!(window_ranges(&files, (0, 60.0), (1, 0.0)), vec![(0, 60.0, 100.0)]); // Ends on a file start.
    }

    #[test]
    fn window_candidates_stay_strictly_inside_the_window() {
        let files = [
            file("a.mp3", vec![sil(49.0, 51.0), sil(70.0, 71.0), sil(90.0, 90.2)], 100.0),
            file("b.mp3", vec![sil(10.0, 11.0), sil(19.5, 20.5)], 80.0),
        ];
        // The silences at the window's own ends are the breaks it runs between; 90.0 is too short.
        assert_eq!(
            window_candidates(&files, (0, 50.0), (1, 20.0), 0.5),
            vec![c(0, 70.0, 71.0), c(1, 0.0, 0.0), c(1, 10.0, 11.0)]
        );
    }

    #[test]
    fn rescan_chapter_picks_the_best_match_for_one_chapter() {
        let chapters = [chapter("Chapter One", CH1), chapter("Chapter Two", CH2)];
        let cands = [c(0, 40.0, 41.0), c(0, 90.0, 91.0)];
        let t = transcripts(&[
            ("as the fishing boats returned", "Old Tom stood at the end of the pier and counted every boat twice."),
            ("The last one came in late and empty.", "Chapter two. Nobody in the village could remember a winter as cold as that one."),
        ]);
        let b = rescan_chapter(&cands, &t, &chapters, 1, 0.4).unwrap();
        assert_eq!((b.file, b.at, b.chapter), (0, 90.5, 1));
        assert!(b.after.starts_with("Chapter two"));
        assert!(rescan_chapter(&cands[..1], &t[..1], &chapters, 1, 0.4).is_none());
    }

    fn brk(file: usize, at: f64, chapter: usize) -> ChapterBreak {
        ChapterBreak { file, at, chapter, score: 1.0, before: String::new(), after: String::new() }
    }

    fn b(file: usize, at: f64, title: &str) -> Break {
        Break { file, at, title: title.to_string() }
    }

    #[test]
    fn chapter_breaks_add_an_opening_and_name_untitled_chapters() {
        let chapters = [chapter("Copyright", "x"), Chapter { title: None, words: vec![] }, chapter("Part 2", "x")];
        let m = ChapterMatch { breaks: vec![brk(0, 5.0, 1), brk(1, 60.0, 2)], missing: vec![0] };
        assert_eq!(
            chapter_breaks(&m, &chapters),
            vec![b(0, 0.0, "Opening"), b(0, 5.0, "Section 2"), b(1, 60.0, "Part 2")]
        );
        // Book starts right on a chapter: no opening.
        let m = ChapterMatch { breaks: vec![brk(0, 0.0, 0)], missing: vec![] };
        assert_eq!(chapter_breaks(&m, &chapters), vec![b(0, 0.0, "Copyright")]);
    }

    #[test]
    fn silence_breaks_split_at_every_candidate() {
        let files = [file("a.mp3", vec![sil(10.0, 13.0), sil(20.0, 20.5)], 100.0), file("b.mp3", vec![sil(30.0, 34.0)], 60.0)];
        assert_eq!(
            silence_breaks(&files, 2.0),
            vec![b(0, 0.0, "Part 1"), b(0, 11.5, "Part 2"), b(1, 0.0, "Part 3"), b(1, 32.0, "Part 4")]
        );
    }

    fn seg(name: &str, pieces: &[(&str, f64, f64)]) -> Segment {
        let pieces = pieces.iter().map(|&(path, start, end)| Piece { path: path.into(), start, end }).collect();
        Segment { name: name.to_string(), pieces }
    }

    #[test]
    fn plan_from_breaks_joins_a_chapter_across_files() {
        let files = [file("a.mp3", vec![], 100.0), file("b.mp3", vec![], 80.0), file("c.mp3", vec![], 50.0)];
        // Unsorted on purpose; the Fall runs from mid a through b into c.
        let breaks = [b(0, 60.0, "The Fall"), b(0, 0.0, "Opening"), b(0, 5.0, "Chapter One")];
        assert_eq!(
            plan_from_breaks(&files, &breaks),
            Ok(vec![
                seg("Opening", &[("a.mp3", 0.0, 5.0)]),
                seg("Chapter One", &[("a.mp3", 5.0, 60.0)]),
                seg("The Fall", &[("a.mp3", 60.0, 100.0), ("b.mp3", 0.0, 80.0), ("c.mp3", 0.0, 50.0)]),
            ])
        );
    }

    #[test]
    fn plan_from_breaks_uses_file_start_breaks() {
        let files = [file("a.mp3", vec![], 100.0), file("b.mp3", vec![], 80.0)];
        let breaks = [b(0, 0.0, "One"), b(1, 0.0, "Two"), b(1, 40.0, "Three")];
        assert_eq!(
            plan_from_breaks(&files, &breaks),
            Ok(vec![seg("One", &[("a.mp3", 0.0, 100.0)]), seg("Two", &[("b.mp3", 0.0, 40.0)]), seg("Three", &[("b.mp3", 40.0, 80.0)])])
        );
    }

    #[test]
    fn plan_from_breaks_opens_with_audio_before_the_first_break() {
        let files = [file("a.mp3", vec![], 100.0), file("b.mp3", vec![], 80.0)];
        assert_eq!(
            plan_from_breaks(&files, &[b(1, 10.0, "One")]),
            Ok(vec![seg("Opening", &[("a.mp3", 0.0, 100.0), ("b.mp3", 0.0, 10.0)]), seg("One", &[("b.mp3", 10.0, 80.0)])])
        );
    }

    #[test]
    fn plan_from_breaks_rejects_bad_breaks() {
        let files = [file("a.mp3", vec![], 100.0)];
        for bad in [
            vec![b(1, 0.0, "x")],                     // No such file.
            vec![b(0, -1.0, "x")],                    // Before the start.
            vec![b(0, 100.0, "x")],                   // At the end: empty segment.
            vec![b(0, f64::NAN, "x")],
            vec![b(0, 0.0, "x"), b(0, 50.0, "y"), b(0, 50.0, "z")], // Duplicate.
        ] {
            assert!(plan_from_breaks(&files, &bad).is_err(), "{:?}", bad);
        }
    }

    #[test]
    fn sanitize_file_name_removes_path_characters() {
        assert_eq!(sanitize_file_name("Part 2: The Fall"), "Part 2 - The Fall");
        assert_eq!(sanitize_file_name(" a/b\\c*?\"<>|\n "), "a_b_c______");
        assert_eq!(sanitize_file_name("..."), "Untitled");
        assert_eq!(sanitize_file_name(&"x".repeat(300)).len(), 120);
    }

    #[test]
    fn sanitize_file_name_avoids_windows_pitfalls() {
        assert_eq!(sanitize_file_name("Con"), "Con_");
        assert_eq!(sanitize_file_name("nul"), "nul_");
        assert_eq!(sanitize_file_name("COM1"), "COM1_");
        assert_eq!(sanitize_file_name("Lpt9"), "Lpt9_");
        assert_eq!(sanitize_file_name("Console"), "Console");
        assert_eq!(sanitize_file_name("COM0"), "COM0");
        // Windows strips trailing dots/spaces, so truncation mustn't leave any.
        let long = format!("{} end", "x".repeat(119));
        assert_eq!(sanitize_file_name(&long), "x".repeat(119));
    }
}
