// The analyze / re-scan / split steps the GUI and the CLI share. Progress leaves as
// events, so each front end shows it its own way (the GUI polls, the CLI draws bars).
use crate::book::{book_info, load_chapters, Chapter};
use crate::chapters::{
    candidates, chapter_breaks, pick_breaks, plan_from_breaks, rescan_chapter, rescan_window, silence_breaks,
    transcribe_candidates, window_candidates, window_ranges, AudioFile, Break, Candidate, ChapterBreak, Segment,
};
use crate::meta::{tracks, Metadata, Track, DEFAULT_TEMPLATE};
use crate::plan::{Findings, Missing, PlanBreak, PlanSilence, Settings};
use crate::transcribe::{find_model, Transcriber};
use crate::{detect_silences, detect_silences_between, split_points, write_segment};
use std::path::{Path, PathBuf};

pub const PREVIEW_SECONDS: f64 = 30.0;
// ponytail: fixed loosening for re-scans; make them settings if one step isn't enough.
const RESCAN_SILENCE_FACTOR: f64 = 0.5;
const RESCAN_NOISE_DB: f64 = 10.0;

pub enum Event {
    Stage { name: String, total: usize }, // A new step; its progress restarts at 0.
    Tick(usize),                          // Items done in the current stage.
    Log(String),
}

pub struct EpubInput {
    pub path: PathBuf,
    pub chapters: Vec<Chapter>,
    pub model: PathBuf,
}

// Reads the EPUB and finds the Whisper model, so a bad EPUB or missing model fails
// before any slow work starts. Also returns (album, artist, year) from the EPUB.
pub fn open_epub(path: &Path) -> Result<(EpubInput, (String, String, String)), String> {
    let chapters = load_chapters(path)?;
    let model = find_model()?;
    let info = book_info(path)?;
    Ok((EpubInput { path: path.to_path_buf(), chapters, model }, info))
}

pub fn default_metadata(info: Option<(String, String, String)>) -> Metadata {
    let mut meta = Metadata { genre: "Audiobook".into(), template: DEFAULT_TEMPLATE.into(), ..Default::default() };
    if let Some((album, artist, year)) = info {
        (meta.album, meta.artist, meta.year) = (album, artist, year);
    }
    meta
}

pub fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().to_string()
}

pub fn chapter_title(chapters: &[Chapter], ch: usize) -> String {
    chapters[ch].title.clone().unwrap_or_else(|| format!("Section {}", ch + 1))
}

pub struct Analysis {
    pub files: Vec<AudioFile>,          // The inputs that could be read.
    pub errors: Vec<(usize, String)>,   // (index into `inputs`, why it was skipped)
    pub findings: Findings,
}

// Detects silences in each input, then suggests chapter breaks: at every silence, or,
// with an EPUB, where Whisper hears a chapter start.
pub fn analyze(
    inputs: &[PathBuf],
    settings: &Settings,
    epub: Option<&EpubInput>,
    emit: &mut dyn FnMut(Event),
) -> Result<Analysis, String> {
    emit(Event::Stage { name: "Detecting silences".into(), total: inputs.len() });
    let mut files = Vec::new();
    let mut errors = Vec::new();
    for (i, path) in inputs.iter().enumerate() {
        let name = file_name(path);
        match detect_silences(path, settings.min_silence, settings.noise_db) {
            Ok((silences, duration)) => {
                let count = split_points(&silences, settings.min_silence, duration).len();
                emit(Event::Log(if epub.is_none() {
                    format!("{}: {} segments", name, count)
                } else {
                    format!("{}: {} candidate silences", name, count.saturating_sub(1))
                }));
                files.push(AudioFile { path: path.clone(), silences, duration });
            }
            Err(e) => {
                emit(Event::Log(format!("{}: {}", name, e)));
                errors.push((i, e));
            }
        }
        emit(Event::Tick(i + 1));
    }

    let findings = match epub {
        None => silence_findings(&files, settings),
        Some(epub) => match_with_epub(&files, epub, settings, emit)?,
    };
    Ok(Analysis { files, errors, findings })
}

fn silence_findings(files: &[AudioFile], settings: &Settings) -> Findings {
    Findings {
        breaks: silence_breaks(files, settings.min_silence)
            .into_iter()
            .map(|b| PlanBreak { file: b.file, at: b.at, title: b.title, score: None })
            .collect(),
        silences: candidates(files, settings.min_silence)
            .iter()
            .map(|c| PlanSilence { file: c.file, at: c.split_at(), before: String::new(), after: String::new() })
            .collect(),
        ..Default::default()
    }
}

// Transcribes around each candidate silence and matches it to the EPUB's chapters.
fn match_with_epub(
    files: &[AudioFile],
    epub: &EpubInput,
    settings: &Settings,
    emit: &mut dyn FnMut(Event),
) -> Result<Findings, String> {
    let cands = candidates(files, settings.min_silence);
    emit(Event::Stage { name: "Transcribing around silences".into(), total: cands.len() });
    emit(Event::Log(format!("Transcribing {} candidate breaks with Whisper...", cands.len())));
    let transcriber = Transcriber::load(&epub.model)?;
    let transcripts = transcribe_candidates(files, &cands, &transcriber, |done, _| emit(Event::Tick(done)))?;

    let chapters = &epub.chapters;
    let m = pick_breaks(&cands, &transcripts, chapters, settings.threshold);
    let breaks = chapter_breaks(&m, chapters)
        .into_iter()
        .map(|b| {
            let score = m.breaks.iter().find(|c| c.file == b.file && c.at == b.at).map(|c| c.score);
            PlanBreak { file: b.file, at: b.at, title: b.title, score }
        })
        .collect();
    emit(Event::Log(format!("Matched {} of {} chapters.", m.breaks.len(), chapters.len())));
    Ok(Findings {
        breaks,
        silences: silence_rows(&cands, &transcripts),
        missing: m.missing.iter().map(|&ch| Missing { chapter: ch, title: chapter_title(chapters, ch), tried: false }).collect(),
        found: m.breaks,
    })
}

fn silence_rows(cands: &[Candidate], transcripts: &[(String, String)]) -> Vec<PlanSilence> {
    cands
        .iter()
        .zip(transcripts)
        .map(|(c, (before, after))| PlanSilence {
            file: c.file,
            at: c.split_at(),
            before: before.trim().to_string(),
            after: after.trim().to_string(),
        })
        .collect()
}

pub struct Rescan {
    pub found: Option<ChapterBreak>,
    pub silences: Vec<PlanSilence>,
}

// Looks again for missing chapter `ch` between the chapters found either side of it,
// with a shorter minimum silence and a higher noise threshold. `found` is every chapter
// matched so far; `files` only needs paths and durations. Fold the result into the
// findings with `Findings::apply_rescan`.
pub fn rescan_one(
    ch: usize,
    files: &[AudioFile],
    found: &[ChapterBreak],
    settings: &Settings,
    epub: &EpubInput,
    transcriber: &Transcriber,
    emit: &mut dyn FnMut(Event),
) -> Result<Rescan, String> {
    let title = chapter_title(&epub.chapters, ch);
    let min_silence = settings.min_silence * RESCAN_SILENCE_FACTOR;
    let noise_db = settings.noise_db + RESCAN_NOISE_DB;
    let (from, to) = rescan_window(found, ch, files);
    let ranges = window_ranges(files, from, to);
    emit(Event::Stage { name: format!("Re-scanning '{}': detecting silences", title), total: ranges.len() });
    emit(Event::Log(format!("Re-scanning '{}' (min silence {}s, noise {}dB)", title, min_silence, noise_db)));

    let mut scan: Vec<AudioFile> =
        files.iter().map(|f| AudioFile { path: f.path.clone(), silences: vec![], duration: f.duration }).collect();
    for (i, &(f, start, end)) in ranges.iter().enumerate() {
        scan[f].silences = detect_silences_between(&files[f].path, start, end, min_silence, noise_db)?;
        emit(Event::Tick(i + 1));
    }
    let cands = window_candidates(&scan, from, to, min_silence);
    emit(Event::Stage { name: format!("Re-scanning '{}': transcribing", title), total: cands.len() });
    let transcripts = transcribe_candidates(&scan, &cands, transcriber, |done, _| emit(Event::Tick(done)))?;
    let hit = rescan_chapter(&cands, &transcripts, &epub.chapters, ch, settings.threshold);
    emit(Event::Log(match &hit {
        Some(b) => format!("Found '{}' (score {:.2}).", title, b.score),
        None => format!("'{}': no match among {} silences.", title, cands.len()),
    }));
    Ok(Rescan { found: hit, silences: silence_rows(&cands, &transcripts) })
}

// Turns the breaks into chapters and names every output file, failing on a bad break or
// an empty naming convention before anything is written.
pub fn prepare_split(files: &[AudioFile], breaks: &[Break], meta: &Metadata) -> Result<(Vec<Segment>, Vec<Track>), String> {
    let segments = plan_from_breaks(files, breaks)?;
    let names = tracks(&segments, meta)?;
    Ok((segments, names))
}

pub struct SplitReport {
    pub written: Vec<PathBuf>,
    pub failed: usize, // Each failure was also reported as a Log event.
}

// Writes one tagged file per chapter into `out_dir` (which must exist).
pub fn write_tracks(segments: &[Segment], names: &[Track], out_dir: &Path, emit: &mut dyn FnMut(Event)) -> SplitReport {
    emit(Event::Stage { name: "Writing files".into(), total: segments.len() });
    let mut report = SplitReport { written: Vec::new(), failed: 0 };
    for (i, (segment, track)) in segments.iter().zip(names).enumerate() {
        match write_segment(&segment.pieces, out_dir, track) {
            Ok(path) => {
                emit(Event::Log(format!("Wrote {}", file_name(&path))));
                report.written.push(path);
            }
            Err(e) => {
                emit(Event::Log(e));
                report.failed += 1;
            }
        }
        emit(Event::Tick(i + 1));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::DEFAULT_TEMPLATE;
    use crate::testutil::{temp_dir, tone_with_gaps};

    fn settings() -> Settings {
        Settings { min_silence: 1.0, noise_db: -40.0, threshold: 0.4 }
    }

    #[test]
    fn analyze_without_an_epub_breaks_at_each_silence() {
        let dir = temp_dir("pipe-analyze");
        let wav = dir.join("book.wav");
        tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
        let mut events = Vec::new();
        let a = analyze(&[wav], &settings(), None, &mut |e| events.push(e)).unwrap();
        assert!(a.errors.is_empty());
        let b = &a.findings.breaks;
        assert_eq!(b.len(), 3, "{:?}", b);
        assert_eq!((b[0].title.as_str(), b[0].at), ("Part 1", 0.0));
        assert!((b[1].at - 5.0).abs() < 0.2 && (b[2].at - 10.0).abs() < 0.2, "{:?}", b);
        assert_eq!(a.findings.silences.len(), 3);
        assert!(matches!(events.first(), Some(Event::Stage { total: 1, .. })));
        assert!(matches!(events.last(), Some(Event::Tick(1))));
    }

    #[test]
    fn analyze_skips_files_it_cannot_read() {
        let dir = temp_dir("pipe-skip");
        let wav = dir.join("book.wav");
        tone_with_gaps(&wav, 10, &[(4, 6)]);
        let a = analyze(&[dir.join("missing.wav"), wav], &settings(), None, &mut |_| {}).unwrap();
        assert_eq!(a.files.len(), 1);
        assert_eq!(a.errors.len(), 1);
        assert_eq!(a.errors[0].0, 0);
    }

    #[test]
    fn splitting_names_each_file_after_its_chapter() {
        let dir = temp_dir("pipe-split");
        let wav = dir.join("book.wav");
        tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
        let a = analyze(&[wav], &settings(), None, &mut |_| {}).unwrap();
        let meta = Metadata { template: DEFAULT_TEMPLATE.into(), ..Default::default() };
        let (segments, names) = prepare_split(&a.files, &a.findings.to_breaks(), &meta).unwrap();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let mut ticks = 0;
        let report = write_tracks(&segments, &names, &out, &mut |e| ticks += matches!(e, Event::Tick(_)) as usize);
        assert_eq!((report.written.len(), report.failed, ticks), (3, 0, 3));
        let mut files: Vec<String> = report.written.iter().map(|p| file_name(p)).collect();
        files.sort();
        assert_eq!(files, ["00 - Part 1.wav", "01 - Part 2.wav", "02 - Part 3.wav"]);
    }

    #[test]
    fn prepare_split_rejects_a_break_outside_its_file() {
        let dir = temp_dir("pipe-bad");
        let wav = dir.join("book.wav");
        tone_with_gaps(&wav, 5, &[]);
        let a = analyze(&[wav], &settings(), None, &mut |_| {}).unwrap();
        let breaks = vec![crate::chapters::Break { file: 0, at: 99.0, title: "Late".into() }];
        assert!(prepare_split(&a.files, &breaks, &Metadata::default()).is_err());
    }

    #[test]
    fn default_metadata_takes_the_epub_info() {
        let m = default_metadata(Some(("Album".into(), "Artist".into(), "1999".into())));
        assert_eq!((m.album.as_str(), m.artist.as_str(), m.year.as_str(), m.genre.as_str()), ("Album", "Artist", "1999", "Audiobook"));
        assert_eq!(m.template, DEFAULT_TEMPLATE);
        assert_eq!(default_metadata(None).album, "");
    }
}
