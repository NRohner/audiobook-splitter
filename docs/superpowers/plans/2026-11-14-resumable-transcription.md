# Resumable Transcription Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cache silence lists and Whisper transcripts on disk so an interrupted `analyze` resumes, and re-analyzing with a different threshold reuses all transcription; add `cache info`/`cache clear` and `--no-cache`.

**Architecture:** A new `src/cache.rs` stores one JSON file per audio file (keyed by a path+size+mtime fingerprint) holding silence lists (keyed by settings) and transcripts (keyed by model and silence position). `pipeline::analyze` and `rescan_one` take a `&Cache`; a new `pipeline::transcribe_cached` looks up hits, transcribes only misses in chunks of 64, and saves after each chunk. Whisper loads lazily, so a fully cached run never loads the model.

**Tech Stack:** Rust 2024, serde_json, existing clap/indicatif/dialoguer CLI. No new dependencies.

Spec: `docs/superpowers/specs/2026-11-14-resumable-transcription-design.md`.

## Global Constraints

- The cache must never change results: a hit returns exactly what a miss would have computed (same settings, same model, same clip length). A changed audio file (size or modified time) changes the fingerprint and misses.
- A cache that can't be found, read or written never fails an analysis: missing, corrupt or newer-`version` files are treated as empty; a failed write produces at most one warning `Log` line per run.
- Cache location: `AUDIOBOOK_CACHE_DIR`, else `~/Library/Caches/audiobook-splitter` (macOS), `$XDG_CACHE_HOME/audiobook-splitter` or `~/.cache/audiobook-splitter` (Linux), `%LOCALAPPDATA%\audiobook-splitter\cache` (Windows). Built from environment variables, no new dependency.
- Files are written to a temp file then renamed. Chunks of 64 candidates (`CHUNK`); save after each chunk.
- CLI: `--no-cache` on `analyze`, `rescan`, `run`; `cache info`; `cache clear [path ...]`. The GUI always uses the cache, no UI change. No size cap or eviction.
- No new native dependencies. No emojis in output. Errors are one line plus the fix, on stderr.
- Tests first (TDD). Part A's tests keep passing; GUI behaviour must not change.
- Deliberate deviation from the spec: when no cache folder can be determined, the cache is silently disabled (no log line); only a failing write logs a warning.

## File Structure

| File | Responsibility |
|---|---|
| `src/cache.rs` (new) | `Cache`: fingerprints, keys, load/store, `info`, `clear`, `default_dir` |
| `src/lib.rs` | `pub mod cache;` |
| `src/pipeline.rs` | `LazyTranscriber`, `transcribe_cached`, cache-aware `analyze` and `rescan_one` |
| `src/gui.rs` | builds one `Cache`, passes it to analyze and rescan |
| `src/cli/mod.rs` | `open_cache`, `--no-cache` on `Detect`, `Cache` subcommand |
| `src/cli/analyze.rs`, `rescan.rs` | pass the cache through |
| `src/cli/cache.rs` (new) | `cache info` / `cache clear` |
| `tests/cli.rs` | integration tests |
| `README.md` | document the cache |

---

### Task 1: The cache module

**Files:**
- Create: `src/cache.rs`
- Modify: `src/lib.rs` (add `pub mod cache;`)

**Interfaces:**
- Consumes: `lib::Silence` (Clone, Serialize, Deserialize), `chapters::CLIP_SECONDS`, `testutil::temp_dir`.
- Produces:
  - `Cache::open(dir: Option<PathBuf>) -> Cache`, `Cache::disabled() -> Cache`, `Cache::default_dir() -> Option<PathBuf>`
  - `cache.silences(audio: &Path, min_silence: f64, noise_db: f64) -> Option<(Vec<Silence>, f64)>`
  - `cache.store_silences(audio: &Path, min_silence: f64, noise_db: f64, silences: &[Silence], duration: f64) -> Option<String>` (returns a warning the first time a write fails)
  - `cache.transcripts(audio: &Path, model: &Path, spans: &[(f64, f64)]) -> Vec<Option<(String, String)>>`
  - `cache.store_transcripts(audio: &Path, model: &Path, items: &[((f64, f64), (String, String))]) -> Option<String>`
  - `cache.info() -> CacheInfo { dir: Option<PathBuf>, entries: usize, bytes: u64 }`
  - `cache.clear(audio: &[PathBuf]) -> Result<usize, String>` (empty slice = everything)

- [ ] **Step 1: Write the failing tests**

Create `src/cache.rs` with only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;
    use std::time::{Duration, SystemTime};

    fn audio(dir: &Path, name: &str, bytes: usize) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, vec![0u8; bytes]).unwrap();
        p
    }

    fn cache_in(dir: &Path) -> Cache {
        Cache::open(Some(dir.join("cache")))
    }

    fn silence(start: f64, end: f64) -> Silence {
        Silence { start, end, duration: end - start }
    }

    fn text(b: &str, a: &str) -> (String, String) {
        (b.to_string(), a.to_string())
    }

    #[test]
    fn silences_round_trip_and_depend_on_the_settings() {
        let dir = temp_dir("cache-sil");
        let wav = audio(&dir, "a.wav", 10);
        let cache = cache_in(&dir);
        assert!(cache.silences(&wav, 2.0, -40.0).is_none());
        assert!(cache.store_silences(&wav, 2.0, -40.0, &[silence(4.0, 6.0)], 15.0).is_none());
        let (found, duration) = cache.silences(&wav, 2.0, -40.0).unwrap();
        assert_eq!((found, duration), (vec![silence(4.0, 6.0)], 15.0));
        assert!(cache.silences(&wav, 1.0, -40.0).is_none(), "min silence is part of the key");
        assert!(cache.silences(&wav, 2.0, -30.0).is_none(), "noise level is part of the key");
    }

    #[test]
    fn transcripts_round_trip_and_depend_on_model_and_position() {
        let dir = temp_dir("cache-tr");
        let wav = audio(&dir, "a.wav", 10);
        let model = audio(&dir, "m.bin", 5);
        let other = audio(&dir, "other.bin", 5);
        let cache = cache_in(&dir);
        let spans = [(1.0, 2.0), (30.0004, 31.0)];
        assert_eq!(cache.transcripts(&wav, &model, &spans), vec![None, None]);
        cache.store_transcripts(&wav, &model, &[((1.0, 2.0), text("b1", "a1"))]);
        assert_eq!(cache.transcripts(&wav, &model, &spans), vec![Some(text("b1", "a1")), None]);
        // Storing more keeps what was there.
        cache.store_transcripts(&wav, &model, &[((30.0, 31.0), text("b2", "a2"))]);
        assert_eq!(cache.transcripts(&wav, &model, &spans), vec![Some(text("b1", "a1")), Some(text("b2", "a2"))]);
        // Positions are keyed to the millisecond, models by name and size.
        assert_eq!(cache.transcripts(&wav, &model, &[(1.0, 2.01)]), vec![None]);
        assert_eq!(cache.transcripts(&wav, &other, &spans), vec![None, None]);
    }

    #[test]
    fn a_changed_audio_file_misses() {
        let dir = temp_dir("cache-fp");
        let wav = audio(&dir, "a.wav", 10);
        let cache = cache_in(&dir);
        cache.store_silences(&wav, 2.0, -40.0, &[silence(4.0, 6.0)], 15.0);
        assert!(cache.silences(&wav, 2.0, -40.0).is_some());

        let file = std::fs::OpenOptions::new().write(true).open(&wav).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(3600)).unwrap();
        drop(file);
        assert!(cache.silences(&wav, 2.0, -40.0).is_none(), "modified time is part of the fingerprint");

        let bigger = audio(&dir, "b.wav", 10);
        cache.store_silences(&bigger, 2.0, -40.0, &[], 1.0);
        std::fs::write(&bigger, vec![0u8; 11]).unwrap();
        assert!(cache.silences(&bigger, 2.0, -40.0).is_none(), "size is part of the fingerprint");
    }

    #[test]
    fn unreadable_cache_files_count_as_empty() {
        let dir = temp_dir("cache-bad");
        let wav = audio(&dir, "a.wav", 10);
        let cache = cache_in(&dir);
        cache.store_silences(&wav, 2.0, -40.0, &[silence(1.0, 2.0)], 9.0);
        let file = std::fs::read_dir(dir.join("cache")).unwrap().next().unwrap().unwrap().path();

        std::fs::write(&file, "not json").unwrap();
        assert!(cache.silences(&wav, 2.0, -40.0).is_none());
        std::fs::write(&file, r#"{"version": 99, "silences": {}, "transcripts": {}}"#).unwrap();
        assert!(cache.silences(&wav, 2.0, -40.0).is_none());

        // Storing over a bad file repairs it.
        assert!(cache.store_silences(&wav, 2.0, -40.0, &[silence(1.0, 2.0)], 9.0).is_none());
        assert!(cache.silences(&wav, 2.0, -40.0).is_some());
    }

    #[test]
    fn a_disabled_cache_stores_nothing() {
        let dir = temp_dir("cache-off");
        let wav = audio(&dir, "a.wav", 10);
        let cache = Cache::disabled();
        assert!(cache.store_silences(&wav, 2.0, -40.0, &[], 1.0).is_none());
        assert!(cache.silences(&wav, 2.0, -40.0).is_none());
        let info = cache.info();
        assert_eq!((info.dir, info.entries, info.bytes), (None, 0, 0));
        assert_eq!(cache.clear(&[]), Ok(0));
    }

    #[test]
    fn a_failing_write_warns_once_and_never_errors() {
        let dir = temp_dir("cache-ro");
        let wav = audio(&dir, "a.wav", 10);
        let blocker = audio(&dir, "blocker", 1); // A file where the cache folder should be.
        let cache = Cache::open(Some(blocker));
        let first = cache.store_silences(&wav, 2.0, -40.0, &[], 1.0);
        assert!(first.is_some_and(|w| w.contains("Cache not saved")));
        assert!(cache.store_silences(&wav, 2.0, -40.0, &[], 1.0).is_none());
        assert!(cache.silences(&wav, 2.0, -40.0).is_none());
    }

    #[test]
    fn info_counts_entries_and_clear_removes_them() {
        let dir = temp_dir("cache-clear");
        let (a, b) = (audio(&dir, "a.wav", 10), audio(&dir, "b.wav", 20));
        let cache = cache_in(&dir);
        cache.store_silences(&a, 2.0, -40.0, &[], 1.0);
        cache.store_silences(&b, 2.0, -40.0, &[], 1.0);
        let info = cache.info();
        assert_eq!(info.entries, 2);
        assert!(info.bytes > 0);

        assert_eq!(cache.clear(&[a.clone()]), Ok(1));
        assert!(cache.silences(&a, 2.0, -40.0).is_none());
        assert!(cache.silences(&b, 2.0, -40.0).is_some());
        assert_eq!(cache.clear(&[a]), Ok(0), "nothing left to clear is not an error");

        assert_eq!(cache.clear(&[]), Ok(1));
        assert_eq!(cache.info().entries, 0);
    }
}
```

- [ ] **Step 2: Run to verify the tests fail**

Add `pub mod cache;` to `src/lib.rs` (next to the other `pub mod` lines), then:

Run: `cargo test --lib cache:: 2>&1 | tail -15`
Expected: compile errors (`Cache` not found).

- [ ] **Step 3: Write the implementation**

Put this above the test module in `src/cache.rs`:

```rust
// On-disk cache of silence lists and Whisper transcripts, so an interrupted or repeated
// analysis picks up where it left off. One JSON file per audio file. A cache that can't be
// read or written never fails an analysis: it just misses.
use crate::chapters::CLIP_SECONDS;
use crate::Silence;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

const CACHE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Entry {
    version: u32,
    // "<min_silence>|<noise_db>" -> the file's silences
    silences: BTreeMap<String, SilenceSet>,
    // "<model name>|<model bytes>|clip<seconds>" -> "<start_ms>|<end_ms>" -> (before, after)
    transcripts: BTreeMap<String, BTreeMap<String, (String, String)>>,
}

#[derive(Serialize, Deserialize)]
struct SilenceSet {
    duration: f64,
    silences: Vec<Silence>,
}

impl Entry {
    fn empty() -> Entry {
        Entry { version: CACHE_VERSION, silences: BTreeMap::new(), transcripts: BTreeMap::new() }
    }
}

pub struct Cache {
    dir: Option<PathBuf>,
    warned: AtomicBool, // A failed write is reported once per run.
}

pub struct CacheInfo {
    pub dir: Option<PathBuf>,
    pub entries: usize,
    pub bytes: u64,
}

// FNV-1a: unlike the standard hasher, stable across Rust versions.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

// Identifies an audio file by where it is, how big it is and when it changed.
// None when the file can't be read (so there is nothing to key on).
fn fingerprint(audio: &Path) -> Option<String> {
    let meta = std::fs::metadata(audio).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let abs = std::path::absolute(audio).ok()?;
    let id = format!("{}|{}|{}", abs.display(), meta.len(), modified);
    Some(format!("{:016x}", fnv1a(id.as_bytes())))
}

fn settings_key(min_silence: f64, noise_db: f64) -> String {
    format!("{:.3}|{:.3}", min_silence, noise_db)
}

fn model_key(model: &Path) -> String {
    let name = model.file_name().unwrap_or_default().to_string_lossy();
    format!("{}|{}|clip{}", name, std::fs::metadata(model).map_or(0, |m| m.len()), CLIP_SECONDS)
}

fn span_key((start, end): (f64, f64)) -> String {
    format!("{}|{}", (start * 1000.0).round() as i64, (end * 1000.0).round() as i64)
}

fn entry_path(dir: &Path, fingerprint: &str) -> PathBuf {
    dir.join(format!("{}.json", fingerprint))
}

fn is_entry(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "json")
}

// What the file holds, or an empty entry if it is missing, unreadable or from another version.
fn load_entry(dir: &Path, fingerprint: &str) -> Entry {
    std::fs::read(entry_path(dir, fingerprint))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Entry>(&bytes).ok())
        .filter(|e| e.version == CACHE_VERSION)
        .unwrap_or_else(Entry::empty)
}

impl Cache {
    pub fn open(dir: Option<PathBuf>) -> Cache {
        Cache { dir, warned: AtomicBool::new(false) }
    }

    pub fn disabled() -> Cache {
        Cache::open(None)
    }

    // `AUDIOBOOK_CACHE_DIR`, else the platform's cache folder.
    pub fn default_dir() -> Option<PathBuf> {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        if let Some(dir) = var("AUDIOBOOK_CACHE_DIR") {
            return Some(dir);
        }
        if cfg!(target_os = "macos") {
            var("HOME").map(|h| h.join("Library/Caches/audiobook-splitter"))
        } else if cfg!(windows) {
            var("LOCALAPPDATA").map(|d| d.join("audiobook-splitter").join("cache"))
        } else {
            var("XDG_CACHE_HOME")
                .or_else(|| var("HOME").map(|h| h.join(".cache")))
                .map(|d| d.join("audiobook-splitter"))
        }
    }

    fn entry(&self, audio: &Path) -> Option<Entry> {
        Some(load_entry(self.dir.as_ref()?, &fingerprint(audio)?))
    }

    // Applies `change` to the audio file's entry and writes it back. Returns a warning the
    // first time a write fails; a disabled cache or an unreadable audio file is silent.
    fn update(&self, audio: &Path, change: impl FnOnce(&mut Entry)) -> Option<String> {
        let dir = self.dir.as_ref()?;
        let fp = fingerprint(audio)?;
        let mut entry = load_entry(dir, &fp);
        change(&mut entry);
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            let tmp = dir.join(format!("{}.json.tmp", fp));
            std::fs::write(&tmp, serde_json::to_vec(&entry).map_err(std::io::Error::other)?)?;
            std::fs::rename(&tmp, entry_path(dir, &fp))
        };
        match write() {
            Ok(()) => None,
            Err(e) => (!self.warned.swap(true, Ordering::Relaxed))
                .then(|| format!("Cache not saved ({}): {}. Continuing without it.", dir.display(), e)),
        }
    }

    pub fn silences(&self, audio: &Path, min_silence: f64, noise_db: f64) -> Option<(Vec<Silence>, f64)> {
        let entry = self.entry(audio)?;
        let set = entry.silences.get(&settings_key(min_silence, noise_db))?;
        Some((set.silences.clone(), set.duration))
    }

    pub fn store_silences(&self, audio: &Path, min_silence: f64, noise_db: f64, silences: &[Silence], duration: f64) -> Option<String> {
        self.update(audio, |e| {
            e.silences.insert(settings_key(min_silence, noise_db), SilenceSet { duration, silences: silences.to_vec() });
        })
    }

    // One entry per span: the cached (before, after) text, or None on a miss.
    pub fn transcripts(&self, audio: &Path, model: &Path, spans: &[(f64, f64)]) -> Vec<Option<(String, String)>> {
        let entry = self.entry(audio);
        let section = entry.as_ref().and_then(|e| e.transcripts.get(&model_key(model)));
        spans.iter().map(|&s| section.and_then(|m| m.get(&span_key(s))).cloned()).collect()
    }

    pub fn store_transcripts(&self, audio: &Path, model: &Path, items: &[((f64, f64), (String, String))]) -> Option<String> {
        self.update(audio, |e| {
            let section = e.transcripts.entry(model_key(model)).or_default();
            for (span, text) in items {
                section.insert(span_key(*span), text.clone());
            }
        })
    }

    pub fn info(&self) -> CacheInfo {
        let files: Vec<_> = self
            .dir
            .as_ref()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| is_entry(p)).collect())
            .unwrap_or_default();
        let bytes = files.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        CacheInfo { dir: self.dir.clone(), entries: files.len(), bytes }
    }

    // Removes the entries of the given audio files, or every entry when `audio` is empty.
    // Returns how many were removed.
    pub fn clear(&self, audio: &[PathBuf]) -> Result<usize, String> {
        let Some(dir) = &self.dir else { return Ok(0) };
        let targets: Vec<PathBuf> = if audio.is_empty() {
            std::fs::read_dir(dir)
                .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| is_entry(p)).collect())
                .unwrap_or_default()
        } else {
            audio.iter().filter_map(|a| fingerprint(a)).map(|fp| entry_path(dir, &fp)).filter(|p| p.is_file()).collect()
        };
        for p in &targets {
            std::fs::remove_file(p).map_err(|e| format!("Failed to remove '{}': {}", p.display(), e))?;
        }
        Ok(targets.len())
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib cache:: 2>&1 | tail -15`
Expected: the 7 `cache::tests` pass. Then `cargo test 2>&1 | grep -E "test result|warning"` shows everything green and no new warnings.

- [ ] **Step 5: Commit**

```bash
git add src/cache.rs src/lib.rs
git commit -m "Add the on-disk cache for silences and transcripts"
```

---

### Task 2: Use the cache in the pipeline, GUI and CLI

**Files:**
- Modify: `src/pipeline.rs`, `src/gui.rs`, `src/cli/mod.rs`, `src/cli/analyze.rs`, `src/cli/rescan.rs`
- Test: `src/pipeline.rs` unit tests, `tests/cli.rs`

**Interfaces:**
- Consumes: Task 1's `Cache`; existing `pipeline::{analyze, rescan_one, match_with_epub}`, `chapters::{transcribe_candidates, Candidate, AudioFile}`, `transcribe::Transcriber`.
- Produces:
  - `pipeline::LazyTranscriber::new(model: &Path) -> LazyTranscriber` and `fn get(&mut self) -> Result<&Transcriber, String>` (loads the model on first use)
  - `pipeline::transcribe_cached(files: &[AudioFile], cands: &[Candidate], model: &Path, cache: &Cache, run: &mut dyn FnMut(&[Candidate], &mut dyn FnMut(usize)) -> Result<Vec<(String, String)>, String>, emit: &mut dyn FnMut(Event)) -> Result<Vec<(String, String)>, String>`
  - `pipeline::analyze(inputs, settings, epub, cache: &Cache, emit)` (new `cache` parameter before `emit`)
  - `pipeline::rescan_one(ch, files, found, settings, epub, transcriber: &mut LazyTranscriber, cache: &Cache, emit)` (transcriber is now `&mut LazyTranscriber`; new `cache`)
  - `cli::open_cache(no_cache: bool) -> Cache`; `Detect.no_cache: bool`; `rescan::Args.no_cache: bool`

- [ ] **Step 1: Write the failing unit tests**

In the `tests` module of `src/pipeline.rs`: add `use crate::cache::Cache;` and `use crate::chapters::Candidate;` to the imports, change the four existing `analyze(&[wav], &settings(), None, ...)` / `analyze(&[dir.join("missing.wav"), wav], ...)` calls to pass `&Cache::disabled()` before the closure, and append:

```rust
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
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib pipeline:: 2>&1 | tail -20`
Expected: compile errors (`transcribe_cached`, new `analyze` signature not found).

- [ ] **Step 3: Implement in `src/pipeline.rs`**

Imports: add `use crate::cache::Cache;`, `use crate::transcribe::{find_model, Transcriber};` (already there), `use std::collections::BTreeMap;`. Add near the top-level items:

```rust
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
```

Change `analyze`'s signature and silence step (the rest of the function is unchanged):

```rust
pub fn analyze(
    inputs: &[PathBuf],
    settings: &Settings,
    epub: Option<&EpubInput>,
    cache: &Cache,
    emit: &mut dyn FnMut(Event),
) -> Result<Analysis, String> {
```

Inside the per-file loop, replace the `match detect_silences(path, ...) {` head with:

```rust
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
```

(the `Ok((silences, duration)) => { ... }` and `Err(e) => { ... }` arms stay as they are), and pass `cache` on: `Some(epub) => match_with_epub(&files, epub, settings, cache, emit)?`.

`match_with_epub` takes `cache: &Cache` before `emit`; replace its transcriber load and transcription:

```rust
    let mut transcriber = LazyTranscriber::new(&epub.model);
    let transcripts = transcribe_cached(
        files,
        &cands,
        &epub.model,
        cache,
        &mut |chunk, tick| transcribe_candidates(files, chunk, transcriber.get()?, |done, _| tick(done)),
        emit,
    )?;
```

(delete the `let transcriber = Transcriber::load(&epub.model)?;` and the old `transcribe_candidates` call).

`rescan_one`: replace the `transcriber: &Transcriber` parameter with `transcriber: &mut LazyTranscriber`, add `cache: &Cache` before `emit`, and replace its transcription:

```rust
    let transcripts = transcribe_cached(
        &scan,
        &cands,
        &epub.model,
        cache,
        &mut |chunk, tick| transcribe_candidates(&scan, chunk, transcriber.get()?, |done, _| tick(done)),
        emit,
    )?;
```

- [ ] **Step 4: Run the unit tests**

Run: `cargo test --lib pipeline:: 2>&1 | tail -20`
Expected: all `pipeline::tests` pass (the lib compiles; `gui.rs` and the CLI still call the old signatures, so build only the lib here with `cargo test --lib`).

- [ ] **Step 5: Update the GUI call sites**

In `src/gui.rs`: import `audiobook_splitter_rust::cache::Cache` and `pipeline::LazyTranscriber` (drop the `Transcriber` import if unused). In `run()`: `let cache = Arc::new(Cache::open(Cache::default_dir()));` and `handle(req, &state, &cache, port);`. Add `cache: &Arc<Cache>` to `handle` and pass it to `start_analyze(r, state, cache)` and `start_rescan(r, state, cache)`. In each of those two functions add `let cache = cache.clone();` before `thread::spawn(move || ...)`. Then:

- `start_analyze`'s thread: `pipeline::analyze(&inputs, &settings, epub.as_ref(), &cache, &mut |e| apply_event(&state, e))`.
- `start_rescan`'s thread: replace `Transcriber::load(&epub.input.model).and_then(|transcriber| { for ch in chapters { ... } Ok(()) })` with

```rust
        let mut transcriber = LazyTranscriber::new(&epub.input.model);
        let result = (|| -> Result<(), String> {
            for ch in chapters {
                let found = state.lock().unwrap().findings.found.clone();
                let r = pipeline::rescan_one(ch, &files, &found, &epub.settings, &epub.input, &mut transcriber, &cache, &mut |e| apply_event(&state, e))?;
                let title = pipeline::chapter_title(&epub.input.chapters, ch);
                state.lock().unwrap().findings.apply_rescan(ch, title, r.found, r.silences);
            }
            Ok(())
        })();
```

- [ ] **Step 6: Update the CLI call sites and add `--no-cache`**

`src/cli/mod.rs`: add `use audiobook_splitter_rust::cache::Cache;` and

```rust
// The shared cache, unless `--no-cache` asked for a run without it.
pub fn open_cache(no_cache: bool) -> Cache {
    if no_cache { Cache::disabled() } else { Cache::open(Cache::default_dir()) }
}
```

Add to the `Detect` struct:

```rust
    /// Don't read or write the transcript cache for this run
    #[arg(long)]
    pub no_cache: bool,
```

`src/cli/analyze.rs` (`build_plan`): `let cache = open_cache(detect.no_cache);` (import `open_cache` from `super`) and `analyze(&inputs, &settings, epub.as_ref(), &cache, &mut |e| reporter.emit(e))`.

`src/cli/rescan.rs`: add to `Args`:

```rust
    /// Don't read or write the transcript cache for this run
    #[arg(long)]
    pub no_cache: bool,
```

and in `run`: replace `let transcriber = Transcriber::load(&epub.model)?;` with

```rust
    let cache = open_cache(args.no_cache);
    let mut transcriber = LazyTranscriber::new(&epub.model);
```

call `rescan_one(ch, &plan.files, &found, &plan.settings, &epub, &mut transcriber, &cache, &mut |e| reporter.emit(e))`, and fix the imports (`pipeline::{chapter_title, open_epub, rescan_one, LazyTranscriber}`, `super::{check_tools, open_cache, open_plan}`; remove `transcribe::Transcriber`).

- [ ] **Step 7: Integration tests for reuse and `--no-cache`**

Append to `tests/cli.rs`:

```rust
// Runs `analyze` with the cache in `cache` and returns stderr, where progress logs go.
fn analyze_stderr(wav: &std::path::Path, plan: &std::path::Path, cache: &std::path::Path, extra: &[&str]) -> String {
    let out = bin()
        .env("AUDIOBOOK_CACHE_DIR", cache)
        .arg("analyze")
        .arg(wav)
        .args(["--min-silence", "1", "-o"])
        .arg(plan)
        .args(extra)
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn a_second_analyze_reuses_the_cached_silences() {
    let dir = common::temp_dir("cache-reuse");
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let cache = dir.join("cache");
    let first = analyze_stderr(&wav, &dir.join("a.json"), &cache, &[]);
    assert!(!first.contains("from cache"), "{}", first);
    let second = analyze_stderr(&wav, &dir.join("b.json"), &cache, &[]);
    assert!(second.contains("silences from cache"), "{}", second);
    assert!(cache.is_dir());
}

#[test]
fn no_cache_leaves_the_cache_alone() {
    let dir = common::temp_dir("cache-off");
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let cache = dir.join("cache");
    analyze_stderr(&wav, &dir.join("a.json"), &cache, &["--no-cache"]);
    assert!(!cache.exists(), "--no-cache must not create the cache folder");
    analyze_stderr(&wav, &dir.join("b.json"), &cache, &[]); // Fills it.
    let again = analyze_stderr(&wav, &dir.join("c.json"), &cache, &["--no-cache"]);
    assert!(!again.contains("from cache"), "{}", again);
}
```

- [ ] **Step 8: Run everything and smoke-test the GUI**

Run: `cargo test 2>&1 | grep -E "test result|warning|FAILED"`
Expected: all green, no warnings.

GUI smoke test (starts the server and opens a browser tab; close it; port 7878 must be free):

```bash
D=$(mktemp -d) && ffmpeg -hide_banner -loglevel error -f lavfi -i "aevalsrc=if(between(t\,4\,6)+between(t\,9\,11)\,0\,sin(2*PI*440*t)):d=15" -y $D/book.wav
cargo build -q && (AUDIOBOOK_CACHE_DIR=$D/cache ./target/debug/audiobook-splitter-rust gui >/dev/null 2>&1 &) ; sleep 1
J='Content-Type: application/json'
for i in 1 2; do
  curl -s -X POST -H "$J" -d "{\"input\":\"$D/book.wav\",\"min_silence\":1.0,\"noise_db\":-40}" http://127.0.0.1:7878/analyze; sleep 3
  curl -s http://127.0.0.1:7878/status | python3 -c 'import json,sys; s=json.load(sys.stdin); print(len(s["breaks"]), s["log"])'
done
pkill -f audiobook-splitter-rust; ls $D/cache
```

Expected: both runs print 3 breaks; the second run's log contains `book.wav: silences from cache`; `$D/cache` holds one `.json` file.

- [ ] **Step 9: Commit**

```bash
git add src/pipeline.rs src/gui.rs src/cli
git add tests/cli.rs
git commit -m "Resume analysis from a shared cache of silences and transcripts"
```

---

### Task 3: `cache info`, `cache clear` and the docs

**Files:**
- Create: `src/cli/cache.rs`
- Modify: `src/cli/mod.rs` (`mod cache;`, `Cache` subcommand variant and dispatch arm), `README.md`
- Test: unit test in `src/cli/cache.rs`, integration tests in `tests/cli.rs`

**Interfaces:**
- Consumes: Task 1's `Cache::{open, default_dir, info, clear}`, `lib::collect_inputs`, `cli::open_cache`.
- Produces: `cache::Args`, `cache::run(Args) -> Result<(), String>`.

- [ ] **Step 1: Write the failing tests**

Create `src/cli/cache.rs` with just:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
```

Append to `tests/cli.rs`:

```rust
#[test]
fn cache_info_and_clear() {
    let dir = common::temp_dir("cache-cmd");
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let cache = dir.join("cache");
    let cmd = |args: &[&str]| {
        let mut c = bin();
        c.env("AUDIOBOOK_CACHE_DIR", &cache).arg("cache").args(args);
        c
    };

    cmd(&["info"]).assert().success().stdout(contains("Cache folder:")).stdout(contains("0 book file(s)"));
    analyze_stderr(&wav, &dir.join("a.json"), &cache, &[]);
    cmd(&["info"]).assert().success().stdout(contains("1 book file(s)"));

    cmd(&["clear", wav.to_str().unwrap()]).assert().success().stdout(contains("Removed 1 cache entry"));
    cmd(&["clear", wav.to_str().unwrap()]).assert().success().stdout(contains("Removed 0 cache entries"));

    analyze_stderr(&wav, &dir.join("b.json"), &cache, &[]);
    cmd(&["clear"]).assert().success().stdout(contains("Removed 1 cache entry"));
    cmd(&["info"]).assert().success().stdout(contains("0 book file(s)"));
}

#[test]
fn cache_clear_rejects_a_missing_path() {
    let dir = common::temp_dir("cache-missing");
    bin()
        .env("AUDIOBOOK_CACHE_DIR", dir.join("cache"))
        .args(["cache", "clear"])
        .arg(dir.join("nothere.wav"))
        .assert()
        .failure()
        .stderr(contains("not a file or folder"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test cli cache_ 2>&1 | tail -10` and `cargo test --bin audiobook-splitter-rust sizes_read 2>&1 | tail -5`
Expected: the integration tests fail (`unrecognized subcommand 'cache'`); the unit test does not compile yet (add `mod cache;` to `src/cli/mod.rs` first, then it fails on `human_bytes`).

- [ ] **Step 3: Implement `src/cli/cache.rs`**

Put this above the test module:

```rust
// `cache`: look at or empty the shared cache of silences and transcripts.
use super::open_cache;
use audiobook_splitter_rust::collect_inputs;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: Sub,
}

#[derive(clap::Subcommand)]
pub enum Sub {
    /// Show where the cache is and how big it is
    Info,
    /// Delete cached silences and transcripts: everything, or only for the given audio files or folders
    Clear {
        /// Audio files or folders whose entries to delete (default: everything)
        paths: Vec<PathBuf>,
    },
}

fn human_bytes(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB"];
    let (mut value, mut unit) = (bytes as f64, 0);
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{} B", bytes) } else { format!("{:.1} {}", value, units[unit]) }
}

pub fn run(args: Args) -> Result<(), String> {
    let cache = open_cache(false);
    match args.cmd {
        Sub::Info => {
            let info = cache.info();
            match &info.dir {
                Some(dir) => {
                    println!("Cache folder: {}", dir.display());
                    println!("Entries:      {} book file(s)", info.entries);
                    println!("Size:         {}", human_bytes(info.bytes));
                }
                None => println!("Cache folder: none available (set AUDIOBOOK_CACHE_DIR to choose one)"),
            }
        }
        Sub::Clear { paths } => {
            let mut audio = Vec::new();
            for p in &paths {
                audio.extend(collect_inputs(p)?);
            }
            let removed = cache.clear(&audio)?;
            println!("Removed {} cache entr{}.", removed, if removed == 1 { "y" } else { "ies" });
        }
    }
    Ok(())
}
```

In `src/cli/mod.rs`: add `mod cache;` to the module list, and to `Cmd`:

```rust
    /// Show or empty the cache of silences and transcripts
    Cache(cache::Args),
```

and the arm `Cmd::Cache(a) => cache::run(a),`. (Inside `cli/mod.rs` the lib's `Cache` type and the local `cache` module do not clash: one is a type, the other a module name.)

- [ ] **Step 4: README**

Add a short section after the CLI section in `README.md`:

````markdown
### Resuming and the cache

Silence detection and Whisper transcription are cached on disk, keyed by each audio file (path, size, modified time) and the model, so an interrupted `analyze` picks up where it stopped (at most the last 64 transcriptions are redone), and re-analyzing with a different `--threshold` or re-running `rescan` reuses everything already transcribed. The GUI uses the same cache.

```bash
audiobook-splitter-rust cache info                  # where it is and how big
audiobook-splitter-rust cache clear                 # delete everything
audiobook-splitter-rust cache clear ~/books/mybook  # delete just this book's entries
audiobook-splitter-rust analyze book/ --no-cache    # ignore the cache for one run
```

The folder is `~/Library/Caches/audiobook-splitter` (macOS), `$XDG_CACHE_HOME` or `~/.cache/audiobook-splitter` (Linux) or `%LOCALAPPDATA%\audiobook-splitter\cache` (Windows); set `AUDIOBOOK_CACHE_DIR` to put it elsewhere. Entries never expire: a changed audio file or a different model simply doesn't match, so `cache clear` is only for reclaiming space.
````

- [ ] **Step 5: Run everything**

Run: `cargo test 2>&1 | grep -E "test result|warning|FAILED"`
Expected: all green, no warnings. Then `cargo run -q -- cache --help` should list `info` and `clear`.

- [ ] **Step 6: Commit**

```bash
git add src/cli README.md tests/cli.rs
git commit -m "Add cache info and cache clear, document the cache"
```

---

## Spec coverage check

| Spec requirement | Task |
|---|---|
| Cache module, fingerprint, FNV-1a, file layout, atomic write, version/corruption tolerance | 1 |
| Location rules and `AUDIOBOOK_CACHE_DIR` | 1 |
| Silence cache in `analyze`, "silences from cache" log | 2 |
| `transcribe_cached` (hits, 64-chunks, save per chunk, order, progress, "Reused N of M" log) | 2 |
| `rescan` uses transcript cache; its silence windows are not cached | 2 |
| Lazy Whisper load (fully cached run never loads the model) | 2 |
| GUI uses the cache with no UI change | 2 |
| `--no-cache` on `analyze`, `rescan`, `run` | 2 |
| `cache info`, `cache clear [path]` | 3 |
| Failed writes warn once, never fail the run | 1 (`update`), 2 (emits the warning) |
| Tests incl. fake-transcriber chunk/failure tests and CLI integration | 1, 2, 3 |
| README docs | 3 |
| Out of scope (size cap, decoded audio, GUI control) | not built |
