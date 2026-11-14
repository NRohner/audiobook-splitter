// The analyze / re-scan / split steps the GUI and the CLI share. Progress leaves as
// events, so each front end shows it its own way (the GUI polls, the CLI draws bars).
use crate::book::{book_info, load_chapters, Chapter};
use crate::cache::Cache;
use crate::chapters::{
    candidates, chapter_breaks, pick_breaks, plan_from_breaks, rescan_chapter, rescan_window, silence_breaks,
    transcribe_candidates, window_candidates, window_ranges, AudioFile, Break, Candidate, ChapterBreak, Segment,
};
use crate::meta::{tracks, Metadata, Track, DEFAULT_TEMPLATE};
use crate::plan::{Findings, Missing, PlanBreak, PlanSilence, Settings};
use crate::transcribe::{find_model, Transcriber};
use crate::{detect_silences, detect_silences_between, split_points, write_segment};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const PREVIEW_SECONDS: f64 = 30.0;
// ponytail: fixed loosening for re-scans; make them settings if one step isn't enough.
const RESCAN_SILENCE_FACTOR: f64 = 0.5;
const RESCAN_NOISE_DB: f64 = 10.0;

// Candidates sent to Whisper at a time; the cache is saved after each, so an interrupted
// run loses at most one chunk.
const CHUNK: usize = 64;

// Loads the Whisper model the first time it is needed, so a run that finds every
// transcript in the cache never loads it.
pub struct LazyTranscriber {
    model: PathBuf,
    loaded: Option<Transcriber>,
}

impl LazyTranscriber {
    pub fn new(model: &Path) -> Self {
        LazyTranscriber { model: model.to_path_buf(), loaded: None }
    }

    pub fn get(&mut self) -> Result<&Transcriber, String> {
        if self.loaded.is_none() {
            self.loaded = Some(Transcriber::load(&self.model)?);
        }
        Ok(self.loaded.as_ref().unwrap())
    }
}

// Transcribes each candidate's surroundings, taking whatever the cache already holds.
// `run` transcribes a chunk of the misses (reporting items done within the chunk through
// its second argument). Results come back in candidate order; each chunk is saved as it
// finishes. Progress counts cached items as done.
pub fn transcribe_cached(
    files: &[AudioFile],
    cands: &[Candidate],
    model: &Path,
    cache: &Cache,
    run: &mut dyn FnMut(&[Candidate], &mut dyn FnMut(usize)) -> Result<Vec<(String, String)>, String>,
    emit: &mut dyn FnMut(Event),
) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<Option<(String, String)>> = vec![None; cands.len()];
    for f in 0..files.len() {
        let idx: Vec<usize> = (0..cands.len()).filter(|&i| cands[i].file == f).collect();
        if idx.is_empty() {
            continue;
        }
        let spans: Vec<(f64, f64)> = idx.iter().map(|&i| (cands[i].start, cands[i].end)).collect();
        for (&i, hit) in idx.iter().zip(cache.transcripts(&files[f].path, model, &spans)) {
            out[i] = hit;
        }
    }
    let hits = out.iter().filter(|o| o.is_some()).count();
    if hits > 0 {
        emit(Event::Log(format!("Reused {} of {} transcripts from cache.", hits, cands.len())));
        emit(Event::Tick(hits));
    }

    let misses: Vec<usize> = (0..cands.len()).filter(|&i| out[i].is_none()).collect();
    let mut done = hits;
    for chunk in misses.chunks(CHUNK) {
        let batch: Vec<Candidate> = chunk.iter().map(|&i| cands[i].clone()).collect();
        let base = done;
        let got = run(&batch, &mut |n| emit(Event::Tick(base + n)))?;
        if got.len() != chunk.len() {
            return Err("Transcription returned the wrong number of results.".into());
        }
        let mut by_file: BTreeMap<usize, Vec<((f64, f64), (String, String))>> = BTreeMap::new();
        for (&i, text) in chunk.iter().zip(got) {
            by_file.entry(cands[i].file).or_default().push(((cands[i].start, cands[i].end), text.clone()));
            out[i] = Some(text);
        }
        for (f, items) in by_file {
            if let Some(warning) = cache.store_transcripts(&files[f].path, model, &items) {
                emit(Event::Log(warning));
            }
        }
        done += chunk.len();
        emit(Event::Tick(done));
    }
    Ok(out.into_iter().map(|o| o.expect("every candidate was looked up or transcribed")).collect())
}

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
    cache: &Cache,
    emit: &mut dyn FnMut(Event),
) -> Result<Analysis, String> {
    emit(Event::Stage { name: "Detecting silences".into(), total: inputs.len() });
    let mut files = Vec::new();
    let mut errors = Vec::new();
    for (i, path) in inputs.iter().enumerate() {
        let name = file_name(path);
        let detected = match cache.silences(path, settings.min_silence, settings.noise_db) {
            Some(hit) => {
                emit(Event::Log(format!("{}: silences from cache", name)));
                Ok(hit)
            }
            None => {
                let found = detect_silences(path, settings.min_silence, settings.noise_db);
                if let Ok((silences, duration)) = &found {
                    if let Some(warning) = cache.store_silences(path, settings.min_silence, settings.noise_db, silences, *duration) {
                        emit(Event::Log(warning));
                    }
                }
                found
            }
        };
        match detected {
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
        Some(epub) => match_with_epub(&files, epub, settings, cache, emit)?,
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
    cache: &Cache,
    emit: &mut dyn FnMut(Event),
) -> Result<Findings, String> {
    let cands = candidates(files, settings.min_silence);
    emit(Event::Stage { name: "Transcribing around silences".into(), total: cands.len() });
    emit(Event::Log(format!("Matching {} candidate breaks to the book...", cands.len())));
    let mut transcriber = LazyTranscriber::new(&epub.model);
    let transcripts = transcribe_cached(
        files,
        &cands,
        &epub.model,
        cache,
        &mut |chunk, tick| transcribe_candidates(files, chunk, transcriber.get()?, |done, _| tick(done)),
        emit,
    )?;

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
    transcriber: &mut LazyTranscriber,
    cache: &Cache,
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
    let transcripts = transcribe_cached(
        &scan,
        &cands,
        &epub.model,
        cache,
        &mut |chunk, tick| transcribe_candidates(&scan, chunk, transcriber.get()?, |done, _| tick(done)),
        emit,
    )?;
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
    use crate::cache::Cache;
    use crate::chapters::Candidate;
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
        let a = analyze(&[wav], &settings(), None, &Cache::disabled(), &mut |e| events.push(e)).unwrap();
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
        let a = analyze(&[dir.join("missing.wav"), wav], &settings(), None, &Cache::disabled(), &mut |_| {}).unwrap();
        assert_eq!(a.files.len(), 1);
        assert_eq!(a.errors.len(), 1);
        assert_eq!(a.errors[0].0, 0);
    }

    #[test]
    fn splitting_names_each_file_after_its_chapter() {
        let dir = temp_dir("pipe-split");
        let wav = dir.join("book.wav");
        tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
        let a = analyze(&[wav], &settings(), None, &Cache::disabled(), &mut |_| {}).unwrap();
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
        let a = analyze(&[wav], &settings(), None, &Cache::disabled(), &mut |_| {}).unwrap();
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

    // One fake audio file and `n` candidate silences in it.
    fn fake_book(name: &str, n: usize) -> (Vec<AudioFile>, Vec<Candidate>, PathBuf, Cache) {
        let dir = temp_dir(name);
        let wav = dir.join("a.wav");
        std::fs::write(&wav, b"audio").unwrap();
        let model = dir.join("m.bin");
        std::fs::write(&model, b"model").unwrap();
        let files = vec![AudioFile { path: wav, silences: vec![], duration: 100_000.0 }];
        let cands = (0..n).map(|i| Candidate { file: 0, start: 10.0 * i as f64 + 1.0, end: 10.0 * i as f64 + 3.0 }).collect();
        (files, cands, model, Cache::open(Some(dir.join("cache"))))
    }

    type Texts = Vec<(String, String)>;

    // Records how many candidates each call got, and answers with text derived from the span.
    fn fake_run<'a>(calls: &'a mut Vec<usize>) -> impl FnMut(&[Candidate], &mut dyn FnMut(usize)) -> Result<Texts, String> + 'a {
        move |chunk, tick| {
            calls.push(chunk.len());
            tick(chunk.len());
            Ok(chunk.iter().map(|c| (format!("b{}", c.start), format!("a{}", c.end))).collect())
        }
    }

    #[test]
    fn transcribe_cached_transcribes_once_then_hits() {
        let (files, cands, model, cache) = fake_book("tc-hit", 3);
        let mut calls = Vec::new();
        let first = transcribe_cached(&files, &cands, &model, &cache, &mut fake_run(&mut calls), &mut |_| {}).unwrap();
        assert_eq!(calls, [3]);
        assert_eq!(first[1], ("b11".to_string(), "a13".to_string()));

        let mut calls = Vec::new();
        let mut logs = Vec::new();
        let second = transcribe_cached(&files, &cands, &model, &cache, &mut fake_run(&mut calls), &mut |e| {
            if let Event::Log(l) = e {
                logs.push(l);
            }
        })
        .unwrap();
        assert!(calls.is_empty(), "everything was cached");
        assert_eq!(second, first);
        assert_eq!(logs, ["Reused 3 of 3 transcripts from cache."]);
    }

    #[test]
    fn transcribe_cached_sends_misses_in_chunks_of_64() {
        for (n, expected) in [(0, vec![]), (1, vec![1]), (64, vec![64]), (65, vec![64, 1]), (130, vec![64, 64, 2])] {
            let (files, cands, model, cache) = fake_book(&format!("tc-chunk-{}", n), n);
            let mut calls = Vec::new();
            let got = transcribe_cached(&files, &cands, &model, &cache, &mut fake_run(&mut calls), &mut |_| {}).unwrap();
            assert_eq!(calls, expected, "n = {}", n);
            assert_eq!(got.len(), n);
        }
    }

    #[test]
    fn transcribe_cached_only_asks_for_misses_and_keeps_order() {
        let (files, cands, model, cache) = fake_book("tc-partial", 3);
        let spans = |i: usize| (cands[i].start, cands[i].end);
        cache.store_transcripts(&files[0].path, &model, &[(spans(0), ("cached0".into(), "x".into())), (spans(2), ("cached2".into(), "y".into()))]);
        let mut calls = Vec::new();
        let got = transcribe_cached(&files, &cands, &model, &cache, &mut fake_run(&mut calls), &mut |_| {}).unwrap();
        assert_eq!(calls, [1]);
        assert_eq!(got[0].0, "cached0");
        assert_eq!(got[1].0, "b11");
        assert_eq!(got[2].0, "cached2");
    }

    #[test]
    fn a_failure_keeps_the_chunks_already_done() {
        let (files, cands, model, cache) = fake_book("tc-fail", 130);
        let mut call = 0;
        let mut flaky = |chunk: &[Candidate], tick: &mut dyn FnMut(usize)| -> Result<Texts, String> {
            call += 1;
            if call == 2 {
                return Err("boom".into());
            }
            tick(chunk.len());
            Ok(chunk.iter().map(|c| (format!("b{}", c.start), format!("a{}", c.end))).collect())
        };
        let err = transcribe_cached(&files, &cands, &model, &cache, &mut flaky, &mut |_| {}).unwrap_err();
        assert_eq!(err, "boom");

        // The first chunk of 64 was saved, so a rerun only has 66 left.
        let mut calls = Vec::new();
        transcribe_cached(&files, &cands, &model, &cache, &mut fake_run(&mut calls), &mut |_| {}).unwrap();
        assert_eq!(calls, [64, 2]);
    }

    #[test]
    fn transcribe_cached_reports_progress_up_to_the_total() {
        let (files, cands, model, cache) = fake_book("tc-tick", 70);
        cache.store_transcripts(&files[0].path, &model, &[((cands[0].start, cands[0].end), ("c".into(), "c".into()))]);
        let mut ticks = Vec::new();
        let mut calls = Vec::new();
        transcribe_cached(&files, &cands, &model, &cache, &mut fake_run(&mut calls), &mut |e| {
            if let Event::Tick(n) = e {
                ticks.push(n);
            }
        })
        .unwrap();
        assert_eq!(ticks.first(), Some(&1), "the cached item counts as done at once");
        assert_eq!(ticks.last(), Some(&70));
        assert!(ticks.windows(2).all(|w| w[0] <= w[1]), "{:?}", ticks);
    }

    #[test]
    fn a_wrong_sized_answer_is_an_error() {
        let (files, cands, model, cache) = fake_book("tc-size", 3);
        let mut short = |_: &[Candidate], _: &mut dyn FnMut(usize)| -> Result<Texts, String> { Ok(vec![]) };
        assert!(transcribe_cached(&files, &cands, &model, &cache, &mut short, &mut |_| {}).is_err());
    }

    #[test]
    fn a_second_analysis_reuses_the_silences() {
        let dir = temp_dir("pipe-cache");
        let wav = dir.join("book.wav");
        tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
        let cache = Cache::open(Some(dir.join("cache")));
        let mut first_logs = Vec::new();
        let first = analyze(&[wav.clone()], &settings(), None, &cache, &mut |e| {
            if let Event::Log(l) = e {
                first_logs.push(l);
            }
        })
        .unwrap();
        assert!(!first_logs.iter().any(|l| l.contains("from cache")), "{:?}", first_logs);

        let mut second_logs = Vec::new();
        let second = analyze(&[wav.clone()], &settings(), None, &cache, &mut |e| {
            if let Event::Log(l) = e {
                second_logs.push(l);
            }
        })
        .unwrap();
        assert!(second_logs.iter().any(|l| l.contains("silences from cache")), "{:?}", second_logs);
        let at = |a: &Analysis| a.findings.breaks.iter().map(|b| (b.title.clone(), b.at)).collect::<Vec<_>>();
        assert_eq!(at(&first), at(&second));
        assert_eq!(first.files[0].duration, second.files[0].duration);

        // Other settings are a different key, so they are detected afresh.
        let mut loose_logs = Vec::new();
        analyze(&[wav], &Settings { min_silence: 0.5, ..settings() }, None, &cache, &mut |e| {
            if let Event::Log(l) = e {
                loose_logs.push(l);
            }
        })
        .unwrap();
        assert!(!loose_logs.iter().any(|l| l.contains("from cache")));
    }
}
