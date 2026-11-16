# CLI Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the CLI every feature the GUI has (analyze, EPUB match, re-scan, chapter and tag editing, preview, split) as scriptable subcommands joined by a saved plan file.

**Architecture:** The analyze, re-scan and split orchestration moves out of `src/gui.rs` into a shared `pipeline` module in the lib; progress leaves it as `Event`s so each front end renders it its own way. A new `plan` module is the JSON file the stages pass along. `gui.rs` becomes a thin HTTP layer over `pipeline`; a new `src/cli/` module (clap) is a second thin layer over the same functions.

**Tech Stack:** Rust 2024, clap 4 (derive), indicatif 0.18, dialoguer 0.12, serde_json; dev: assert_cmd 2, predicates 3. FFmpeg/FFprobe on `PATH` for tests.

Spec: `docs/superpowers/specs/2026-11-14-cli-parity-design.md`.

## Global Constraints

- Plan file: `"version": 1`; paths stored absolute; a newer `version` is a clear error; audio files must exist and match the recorded duration within 0.5 s before `edit`, `rescan`, `preview`, `split` run.
- No new native dependencies. New crates: `clap` (derive), `indicatif`, `dialoguer`; dev `assert_cmd`, `predicates`.
- With no arguments the binary prints help (exit 2); the GUI needs `gui`; `--cli` stays as a hidden alias for `run`.
- Fail fast before slow work: missing `ffmpeg`/`ffprobe`, missing Whisper model, unreadable EPUB, bad flag values. Errors are one line plus the fix, on stderr; exit code non-zero.
- A plan file is written to a temp file and renamed, so Ctrl-C leaves no partial plan.
- No emojis in any output. GUI behaviour must not change.
- Tests first (TDD). Existing tests keep passing after every task.
- Deviations from the spec, all deliberate: the plan stores `found` (matched chapter indices, needed so `rescan` works after editing) and `epub` but not the model path (resolved by `find_model()` at run time, so a plan survives moving machines); `created` is Unix seconds; `preview` plays only with `--play` (no audio-device detection).

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | new dependencies |
| `src/lib.rs` | `mod` lines, `probe_duration`, serde derives on `Silence` |
| `src/chapters.rs` | serde derives on `AudioFile`, `ChapterBreak` |
| `src/testutil.rs` (new, test-only) | `temp_dir`, `tone_with_gaps` for unit tests |
| `src/plan.rs` (new) | `Plan`, `Settings`, `Findings`, row types, load/save/check |
| `src/pipeline.rs` (new) | `Event`, `analyze`, `rescan_one`, `prepare_split`, `write_tracks` |
| `src/gui.rs` | uses `pipeline`/`plan`; drops its own copies |
| `src/main.rs` | `--pick` check, then `cli::run` |
| `src/cli/mod.rs` | clap definitions, dispatch, shared helpers |
| `src/cli/time.rs` | `clock`, `parse_time` |
| `src/cli/progress.rs` | `Reporter`: `Event` to indicatif bars |
| `src/cli/prompt.rs` | terminal checks and prompts |
| `src/cli/analyze.rs`, `split.rs`, `edit.rs`, `rescan.rs`, `preview.rs`, `run.rs` | one subcommand each |
| `tests/common/mod.rs`, `tests/cli.rs` (new) | CLI integration tests |
| `README.md`, `Makefile` | minimum edits to stay correct |

---

### Task 1: Foundations and the plan file

**Files:**
- Modify: `Cargo.toml`, `src/lib.rs`, `src/chapters.rs`
- Create: `src/testutil.rs`, `src/plan.rs`

**Interfaces:**
- Produces (used by every later task):
  - `lib::probe_duration(input: &Path) -> Result<f64, String>`
  - `plan::Settings { min_silence: f64, noise_db: f64, threshold: f64 }` (`Default` = 2.0, -40.0, 0.4; `fn validate(&self) -> Result<(), String>`)
  - `plan::PlanBreak { file: usize, at: f64, title: String, score: Option<f64> }`
  - `plan::PlanSilence { file: usize, at: f64, before: String, after: String }`
  - `plan::Missing { chapter: usize, title: String, tried: bool }`
  - `plan::Findings { breaks: Vec<PlanBreak>, silences: Vec<PlanSilence>, missing: Vec<Missing>, found: Vec<ChapterBreak> }` with `fn apply_rescan(&mut self, ch: usize, title: String, found: Option<ChapterBreak>, silences: Vec<PlanSilence>)` and `fn to_breaks(&self) -> Vec<Break>`
  - `plan::Plan { version, created, settings, epub: Option<PathBuf>, files: Vec<AudioFile>, findings: Findings (flattened), metadata: Metadata }` with `Plan::new(files, settings, epub, findings, metadata)`, `Plan::load(&Path)`, `plan.save(&Path)`, `plan.check_files()`, `plan.sort_breaks()`
  - `testutil::temp_dir(name) -> PathBuf`, `testutil::tone_with_gaps(path: &Path, seconds: u32, gaps: &[(u32, u32)])`

- [ ] **Step 1: Add dependencies**

In `Cargo.toml`, add under `[dependencies]` (keep the existing lines):

```toml
clap = { version = "4", features = ["derive"] }
dialoguer = "0.12"
indicatif = "0.18"
```

and under `[dev-dependencies]` (next to `zip`):

```toml
assert_cmd = "2"
predicates = "3"
```

- [ ] **Step 2: Derive serde on the types the plan stores**

`src/lib.rs`: change the `Silence` derive and add the import (top of file, after the existing `use` lines):

```rust
use serde::{Deserialize, Serialize};
```

```rust
// A detected silence region, in seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Silence {
```

`src/chapters.rs`: change the two derives.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioFile {
```

```rust
// The text fields are only for display (the GUI polls them), so the plan file leaves them out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterBreak {
    pub file: usize,
    pub at: f64,
    pub chapter: usize,
    pub score: f64,
    #[serde(skip)]
    pub before: String,
    #[serde(skip)]
    pub after: String,
}
```

(`serde::{Deserialize, Serialize}` is already imported in `chapters.rs`.)

- [ ] **Step 3: Extract `probe_duration` in `src/lib.rs`**

Replace `detect_silences` with:

```rust
// Total length of `input` in seconds, from FFprobe.
pub fn probe_duration(input: &Path) -> Result<f64, String> {
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(input)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ffprobe. Is FFprobe installed and on your PATH? ({})", e))?;
    if !probe.status.success() {
        return Err(format!("FFprobe failed:\n{}", String::from_utf8_lossy(&probe.stderr)));
    }
    String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse::<f64>()
        .map_err(|e| format!("Failed to parse audio duration: {}", e))
}

// Runs FFmpeg silence detection and FFprobe, returning the silences and total duration.
pub fn detect_silences(
    input: &Path,
    min_silence: f64,
    noise_db: f64,
) -> Result<(Vec<Silence>, f64), String> {
    let silences = silencedetect(input, &[], min_silence, noise_db)?;
    Ok((silences, probe_duration(input)?))
}
```

Add the module lines near the top of `src/lib.rs`:

```rust
pub mod pipeline;
pub mod plan;
#[cfg(test)]
mod testutil;
```

(`pipeline` is created in Task 2; until then create an empty `src/pipeline.rs` so the crate compiles.)

- [ ] **Step 4: Create the test helpers**

`src/testutil.rs`:

```rust
// Shared by unit tests: temp folders and generated audio.
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abs-test-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// A WAV `seconds` long: silent during each `(start, end)` gap, a 440 Hz tone elsewhere.
pub fn tone_with_gaps(path: &Path, seconds: u32, gaps: &[(u32, u32)]) {
    let silent = if gaps.is_empty() {
        "0".to_string()
    } else {
        gaps.iter().map(|(a, b)| format!("between(t\\,{}\\,{})", a, b)).collect::<Vec<_>>().join("+")
    };
    let expr = format!("aevalsrc=if({}\\,0\\,sin(2*PI*440*t)):d={}", silent, seconds);
    let made = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &expr, "-y"])
        .arg(path)
        .status()
        .unwrap();
    assert!(made.success());
}
```

- [ ] **Step 5: Write the failing tests**

Create `src/plan.rs` with only the test module for now (the types come in Step 7):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{temp_dir, tone_with_gaps};

    fn sample(path: PathBuf, duration: f64) -> Plan {
        Plan::new(
            vec![AudioFile { path, silences: vec![], duration }],
            Settings::default(),
            None,
            Findings::default(),
            Metadata::default(),
        )
    }

    fn found(chapter: usize, at: f64) -> ChapterBreak {
        ChapterBreak { file: 0, at, chapter, score: 1.5, before: String::new(), after: String::new() }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = temp_dir("plan-roundtrip");
        let mut plan = sample("a.mp3".into(), 10.0);
        plan.findings.breaks.push(PlanBreak { file: 0, at: 0.0, title: "Opening".into(), score: None });
        plan.findings.found.push(found(2, 5.0));
        plan.save(&dir.join("sub/plan.json")).unwrap();
        let back = Plan::load(&dir.join("sub/plan.json")).unwrap();
        assert_eq!(serde_json::to_value(&plan).unwrap(), serde_json::to_value(&back).unwrap());
        assert_eq!(back.findings.found[0].chapter, 2);
        assert!(back.files[0].path.is_absolute());
    }

    #[test]
    fn save_leaves_no_temp_file() {
        let dir = temp_dir("plan-tmp");
        sample("a.mp3".into(), 10.0).save(&dir.join("plan.json")).unwrap();
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["plan.json"]);
    }

    #[test]
    fn load_rejects_newer_versions_and_garbage() {
        let dir = temp_dir("plan-bad");
        std::fs::write(dir.join("new.json"), r#"{"version": 99, "whatever": true}"#).unwrap();
        assert!(Plan::load(&dir.join("new.json")).unwrap_err().contains("newer version"));
        std::fs::write(dir.join("junk.json"), "not json").unwrap();
        assert!(Plan::load(&dir.join("junk.json")).unwrap_err().contains("not a valid plan"));
        assert!(Plan::load(&dir.join("missing.json")).is_err());
    }

    #[test]
    fn check_files_rejects_missing_and_changed_audio() {
        let dir = temp_dir("plan-check");
        let wav = dir.join("a.wav");
        tone_with_gaps(&wav, 6, &[]);
        sample(wav.clone(), 6.0).check_files().unwrap();
        let err = sample(wav.clone(), 20.0).check_files().unwrap_err();
        assert!(err.contains("changed") && err.contains("a.wav"), "{}", err);
        let err = sample(dir.join("gone.wav"), 6.0).check_files().unwrap_err();
        assert!(err.contains("gone.wav") && err.contains("missing"), "{}", err);
    }

    #[test]
    fn settings_validate_like_the_gui_does() {
        assert!(Settings::default().validate().is_ok());
        assert!(Settings { min_silence: 0.0, ..Settings::default() }.validate().is_err());
        assert!(Settings { noise_db: f64::NAN, ..Settings::default() }.validate().is_err());
        assert!(Settings { threshold: 1.5, ..Settings::default() }.validate().is_err());
    }

    #[test]
    fn apply_rescan_adds_a_find_and_clears_the_missing_entry() {
        let mut f = Findings::default();
        f.missing = vec![Missing { chapter: 3, title: "Storm".into(), tried: false }];
        f.silences = vec![PlanSilence { file: 0, at: 9.0, before: "a".into(), after: "b".into() }];
        let rows = vec![
            PlanSilence { file: 0, at: 9.0, before: "dup".into(), after: "dup".into() },
            PlanSilence { file: 0, at: 4.0, before: "c".into(), after: "d".into() },
        ];
        f.apply_rescan(3, "Storm".into(), Some(found(3, 4.0)), rows);
        assert!(f.missing.is_empty());
        assert_eq!((f.breaks[0].title.as_str(), f.breaks[0].score), ("Storm", Some(1.5)));
        assert_eq!(f.found.len(), 1);
        let at: Vec<f64> = f.silences.iter().map(|s| s.at).collect();
        assert_eq!(at, [4.0, 9.0]); // Sorted, and the duplicate at 9.0 was not added.
        assert_eq!(f.silences[1].before, "a");
    }

    #[test]
    fn apply_rescan_without_a_find_marks_the_chapter_tried() {
        let mut f = Findings::default();
        f.missing = vec![Missing { chapter: 3, title: "Storm".into(), tried: false }];
        f.apply_rescan(3, "Storm".into(), None, vec![]);
        assert!(f.missing[0].tried && f.breaks.is_empty());
    }

    #[test]
    fn to_breaks_and_sort_breaks_follow_position() {
        let mut plan = sample("a.mp3".into(), 100.0);
        for (at, title) in [(50.0, "B"), (0.0, "A")] {
            plan.findings.breaks.push(PlanBreak { file: 0, at, title: title.into(), score: None });
        }
        plan.sort_breaks();
        let titles: Vec<_> = plan.findings.to_breaks().into_iter().map(|b| b.title).collect();
        assert_eq!(titles, ["A", "B"]);
    }
}
```

- [ ] **Step 6: Run to verify the tests fail**

Run: `cargo test --lib plan:: 2>&1 | tail -20`
Expected: compile errors (`Plan`, `Settings`, ... not found).

- [ ] **Step 7: Write `src/plan.rs`**

Put this above the `#[cfg(test)]` module:

```rust
// The analysis saved to disk, so a long run can be edited and split later.
use crate::chapters::{AudioFile, Break, ChapterBreak};
use crate::meta::Metadata;
use crate::probe_duration;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const PLAN_VERSION: u32 = 1;
// How far an audio file's length may drift from the plan's before it counts as changed.
const DURATION_TOLERANCE: f64 = 0.5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub min_silence: f64,
    pub noise_db: f64,
    pub threshold: f64, // Minimum EPUB match score.
}

impl Default for Settings {
    fn default() -> Self {
        Settings { min_silence: 2.0, noise_db: -40.0, threshold: 0.4 }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !(self.min_silence > 0.0) {
            return Err("Minimum silence must be a positive number.".into());
        }
        if !self.noise_db.is_finite() {
            return Err("Noise threshold must be a number.".into());
        }
        if !(0.0..=1.0).contains(&self.threshold) {
            return Err("Match threshold must be between 0 and 1.".into());
        }
        Ok(())
    }
}

// A chapter start suggested by the analysis, then edited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanBreak {
    pub file: usize, // Index into the plan's files.
    pub at: f64,
    pub title: String,
    pub score: Option<f64>, // EPUB match score.
}

// A silence that can become a chapter break, with what was heard either side (EPUB only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanSilence {
    pub file: usize,
    pub at: f64,
    pub before: String,
    pub after: String,
}

// An EPUB chapter with no break; `tried` once a re-scan also came up empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Missing {
    pub chapter: usize,
    pub title: String,
    pub tried: bool,
}

// What an analysis found. `found` is the matched chapters as the matcher saw them (kept
// unedited, so a re-scan knows where each neighbouring chapter was).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Findings {
    pub breaks: Vec<PlanBreak>,
    pub silences: Vec<PlanSilence>,
    pub missing: Vec<Missing>,
    pub found: Vec<ChapterBreak>,
}

impl Findings {
    // Folds the result of re-scanning chapter `ch` into the findings. New breaks are
    // appended, not sorted: the GUI picks them up by position.
    pub fn apply_rescan(&mut self, ch: usize, title: String, found: Option<ChapterBreak>, silences: Vec<PlanSilence>) {
        for row in silences {
            if !self.silences.iter().any(|x| x.file == row.file && (x.at - row.at).abs() < 0.001) {
                self.silences.push(row);
            }
        }
        self.silences.sort_by(|a, b| (a.file, a.at).partial_cmp(&(b.file, b.at)).unwrap());
        match found {
            Some(b) => {
                self.breaks.push(PlanBreak { file: b.file, at: b.at, title, score: Some(b.score) });
                self.missing.retain(|m| m.chapter != ch);
                self.found.push(b);
            }
            None => {
                if let Some(m) = self.missing.iter_mut().find(|m| m.chapter == ch) {
                    m.tried = true;
                }
            }
        }
    }

    pub fn to_breaks(&self) -> Vec<Break> {
        self.breaks.iter().map(|b| Break { file: b.file, at: b.at, title: b.title.clone() }).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub version: u32,
    pub created: u64, // Unix seconds.
    pub settings: Settings,
    pub epub: Option<PathBuf>,
    pub files: Vec<AudioFile>,
    #[serde(flatten)]
    pub findings: Findings,
    pub metadata: Metadata,
}

impl Plan {
    // Paths are stored absolute, so the plan works from any folder.
    pub fn new(files: Vec<AudioFile>, settings: Settings, epub: Option<PathBuf>, findings: Findings, metadata: Metadata) -> Plan {
        let abs = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
        Plan {
            version: PLAN_VERSION,
            created: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            settings,
            epub: epub.map(abs),
            files: files.into_iter().map(|f| AudioFile { path: abs(f.path), ..f }).collect(),
            findings,
            metadata,
        }
    }

    pub fn load(path: &Path) -> Result<Plan, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("Failed to read plan '{}': {}", path.display(), e))?;
        let invalid = |e: serde_json::Error| format!("'{}' is not a valid plan file: {}", path.display(), e);
        let value: Value = serde_json::from_str(&text).map_err(invalid)?;
        if let Some(v) = value.get("version").and_then(Value::as_u64) {
            if v > PLAN_VERSION as u64 {
                return Err(format!(
                    "'{}' was made by a newer version of this app (plan version {}; this one reads up to {}). Update the app.",
                    path.display(),
                    v,
                    PLAN_VERSION
                ));
            }
        }
        serde_json::from_value(value).map_err(invalid)
    }

    // Writes to a temp file, then renames, so an interrupted save never leaves half a plan.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_file_name(format!("{}.tmp", path.file_name().unwrap_or_default().to_string_lossy()));
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&tmp, &json)?;
            std::fs::rename(&tmp, path)
        };
        write().map_err(|e| format!("Failed to write plan '{}': {}", path.display(), e))
    }

    // Every audio file must still be there and the length the plan recorded.
    pub fn check_files(&self) -> Result<(), String> {
        for f in &self.files {
            let name = f.path.file_name().unwrap_or_default().to_string_lossy();
            if !f.path.is_file() {
                return Err(format!("Audio file '{}' from the plan is missing. Analyze again.", f.path.display()));
            }
            let now = probe_duration(&f.path)?;
            if (now - f.duration).abs() > DURATION_TOLERANCE {
                return Err(format!(
                    "'{}' has changed since the plan was made (was {:.1}s, now {:.1}s). Analyze again.",
                    name, f.duration, now
                ));
            }
        }
        Ok(())
    }

    pub fn sort_breaks(&mut self) {
        self.findings.breaks.sort_by(|a, b| (a.file, a.at).partial_cmp(&(b.file, b.at)).unwrap());
    }
}
```

(`PathBuf` is used by the tests through `use super::*`.)

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --lib 2>&1 | tail -15`
Expected: all pass, including the 7 new `plan::tests`. If `probe_duration` on the generated WAV disagrees with 6.0 by more than 0.5 s, print it and fix the test, not the tolerance.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/chapters.rs src/testutil.rs src/plan.rs src/pipeline.rs
git commit -m "Add the plan file and a shared duration probe"
```

---

### Task 2: The shared pipeline

**Files:**
- Create: `src/pipeline.rs` (replace the empty placeholder)

**Interfaces:**
- Consumes: everything Task 1 produced.
- Produces:
  - `pipeline::Event` = `Stage { name: String, total: usize } | Tick(usize) | Log(String)`
  - `pipeline::PREVIEW_SECONDS: f64` (30.0)
  - `pipeline::EpubInput { path: PathBuf, chapters: Vec<Chapter>, model: PathBuf }`
  - `pipeline::open_epub(&Path) -> Result<(EpubInput, (String, String, String)), String>` (second item = album, artist, year)
  - `pipeline::default_metadata(info: Option<(String, String, String)>) -> Metadata`
  - `pipeline::file_name(&Path) -> String`, `pipeline::chapter_title(&[Chapter], usize) -> String`
  - `pipeline::Analysis { files: Vec<AudioFile>, errors: Vec<(usize, String)>, findings: Findings }`
  - `pipeline::analyze(inputs: &[PathBuf], settings: &Settings, epub: Option<&EpubInput>, emit: &mut dyn FnMut(Event)) -> Result<Analysis, String>`
  - `pipeline::Rescan { found: Option<ChapterBreak>, silences: Vec<PlanSilence> }`
  - `pipeline::rescan_one(ch: usize, files: &[AudioFile], found: &[ChapterBreak], settings: &Settings, epub: &EpubInput, transcriber: &Transcriber, emit: &mut dyn FnMut(Event)) -> Result<Rescan, String>`
  - `pipeline::prepare_split(files: &[AudioFile], breaks: &[Break], meta: &Metadata) -> Result<(Vec<Segment>, Vec<Track>), String>`
  - `pipeline::SplitReport { written: Vec<PathBuf>, failed: usize }`
  - `pipeline::write_tracks(segments: &[Segment], names: &[Track], out_dir: &Path, emit: &mut dyn FnMut(Event)) -> SplitReport`

- [ ] **Step 1: Write the failing tests**

Write `src/pipeline.rs` containing only:

```rust
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
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib pipeline:: 2>&1 | tail -20`
Expected: compile errors (`analyze` etc. not found).

- [ ] **Step 3: Write the implementation**

Put this above the test module in `src/pipeline.rs`. It is the logic of `start_analyze`, `match_with_epub`, `rescan_one` and `start_split` in `src/gui.rs`, with `Status` replaced by `emit`.

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib 2>&1 | tail -15`
Expected: all pass. (`gui.rs` still has its own copies, so the binary may warn about nothing yet; the lib compiles on its own.)

- [ ] **Step 5: Commit**

```bash
git add src/pipeline.rs
git commit -m "Add a shared pipeline for analyze, re-scan and split"
```

---

### Task 3: Move the GUI onto the pipeline

**Files:**
- Modify: `src/gui.rs` (imports at lines 5-25, types at 27-97, `start_analyze`/`match_with_epub`/`silence_rows`/`chapter_title`/`start_rescan`/`rescan_one`/`start_split`/`file_name` at 283-590)
- Test: existing `gui::tests` plus a manual smoke run

**Interfaces:**
- Consumes: everything from Tasks 1-2.
- Produces: no new public items. The page's JSON contract (`/status`, `/analyze`, `/rescan`, `/split`) must not change.

This is a refactor: behaviour stays identical, so the safety net is the existing tests plus the smoke test in Step 6. Do not touch `ui.html`.

- [ ] **Step 1: Replace the imports and constants (lines 5-25)**

```rust
use audiobook_splitter_rust::chapters::{AudioFile, Break};
use audiobook_splitter_rust::meta::Metadata;
use audiobook_splitter_rust::pipeline::{self, default_metadata, file_name, open_epub, EpubInput, Event, PREVIEW_SECONDS};
use audiobook_splitter_rust::plan::{Findings, Settings};
use audiobook_splitter_rust::transcribe::Transcriber;
use audiobook_splitter_rust::{collect_inputs, preview_clip, user_path, AUDIO_EXTENSIONS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use tiny_http::{Header, Method, Request, Response, Server};

const PAGE: &str = include_str!("ui.html");
const DEFAULT_PORT: u16 = 7878;
```

In `clip()` replace `CLIP_SECONDS` with `PREVIEW_SECONDS`.

- [ ] **Step 2: Replace the row types, `EpubRun` and `Status` (lines 27-97)**

Delete `BreakRow`, `SilenceRow`, `MissingRow` (the plan's `PlanBreak`/`PlanSilence`/`Missing` serialize to the same JSON). Keep `FileRow` and `AudioRow`. New versions:

```rust
// What an EPUB analysis used, kept so missing chapters can be re-scanned.
struct EpubRun {
    input: EpubInput,
    settings: Settings,
}

#[derive(Serialize, Default)]
struct Status {
    running: bool,
    phase: &'static str, // "analyze", "split" or "" before anything has run
    stage: String,       // What the progress bar is counting right now.
    done: usize,
    total: usize,
    run: usize,           // Counts analyses, so the page knows when to reload its editor.
    files: Vec<FileRow>,  // Every input, with any error.
    audio: Vec<AudioRow>, // The analyzed files; `file` in breaks and silences indexes these.
    #[serde(flatten)]
    findings: Findings, // breaks, silences and missing for the page (`found` too, which it ignores).
    meta: Metadata,     // Suggested tags.
    log: Vec<String>,
    #[serde(skip)]
    analyzed: Vec<AudioFile>, // Same order as `audio`.
    #[serde(skip)]
    epub: Option<Arc<EpubRun>>,
}
```

Keep `AnalyzeReq`, `SplitReq`, `RescanReq`, `BrowseReq`, `default_threshold` as they are.

- [ ] **Step 3: Replace `start_analyze` through `start_split` (lines 283-584)**

Delete `match_with_epub`, `silence_rows`, `chapter_title`, `rescan_one`. The replacements:

```rust
// Applies a pipeline event to the status the page polls.
fn apply_event(state: &Shared, event: Event) {
    let mut s = state.lock().unwrap();
    match event {
        Event::Stage { name, total } => {
            s.stage = name;
            s.done = 0;
            s.total = total;
        }
        Event::Tick(done) => s.done = done,
        Event::Log(line) => s.log.push(line),
    }
}

fn start_analyze(req: AnalyzeReq, state: &Shared) -> Result<Value, String> {
    let settings = Settings { min_silence: req.min_silence, noise_db: req.noise_db, threshold: req.threshold };
    settings.validate()?;
    let inputs = collect_inputs(&user_path(&req.input))?;
    if inputs.is_empty() {
        return Err("No supported audio files found in that folder.".into());
    }
    // Fail fast on a bad EPUB or missing model, before any slow work starts.
    let epub_path = user_path(&req.epub);
    let (epub, meta) = if epub_path.as_os_str().is_empty() {
        (None, default_metadata(None))
    } else {
        let (epub, info) = open_epub(&epub_path)?;
        (Some(epub), default_metadata(Some(info)))
    };

    {
        let mut s = state.lock().unwrap();
        if s.running {
            return Err("A job is already running.".into());
        }
        *s = Status {
            run: s.run + 1,
            meta,
            running: true,
            phase: "analyze",
            stage: "Detecting silences".into(),
            total: inputs.len(),
            files: inputs.iter().map(|p| FileRow { name: file_name(p), error: None }).collect(),
            ..Default::default()
        };
        s.log.push(format!("Analyzing {} file(s): min silence {}s, noise {}dB", inputs.len(), settings.min_silence, settings.noise_db));
        if let Some(epub) = &epub {
            s.log.push(format!("EPUB: {} chapters", epub.chapters.len()));
        }
    }

    let state = state.clone();
    thread::spawn(move || {
        let result = pipeline::analyze(&inputs, &settings, epub.as_ref(), &mut |e| apply_event(&state, e));
        let mut s = state.lock().unwrap();
        match result {
            Ok(a) => {
                for (i, e) in a.errors {
                    s.files[i].error = Some(e);
                }
                s.audio = a.files.iter().map(|f| AudioRow { name: file_name(&f.path), duration: f.duration }).collect();
                s.findings = a.findings;
                s.analyzed = a.files;
                s.epub = epub.map(|input| Arc::new(EpubRun { input, settings }));
                s.log.push("Analysis complete.".into());
            }
            Err(e) => s.log.push(format!("Error: {}", e)),
        }
        s.running = false;
    });
    Ok(json!({}))
}

// Looks again for missing chapters, in book order, between the chapters found either
// side of each, with a shorter minimum silence and a higher noise threshold.
fn start_rescan(req: RescanReq, state: &Shared) -> Result<Value, String> {
    let (epub, files, mut chapters) = {
        let mut s = state.lock().unwrap();
        if s.running {
            return Err("A job is already running.".into());
        }
        let epub = s.epub.clone().ok_or("Analyze with an EPUB first.")?;
        let mut chapters = req.chapters;
        chapters.retain(|ch| s.findings.missing.iter().any(|m| m.chapter == *ch));
        if chapters.is_empty() {
            return Err("Those chapters aren't missing any more.".into());
        }
        let files = s.analyzed.clone();
        s.running = true;
        s.phase = "analyze";
        (epub, files, chapters)
    };
    chapters.sort();
    chapters.dedup();

    let state = state.clone();
    thread::spawn(move || {
        let result = Transcriber::load(&epub.input.model).and_then(|transcriber| {
            for ch in chapters {
                let found = state.lock().unwrap().findings.found.clone();
                let r = pipeline::rescan_one(ch, &files, &found, &epub.settings, &epub.input, &transcriber, &mut |e| apply_event(&state, e))?;
                let title = pipeline::chapter_title(&epub.input.chapters, ch);
                state.lock().unwrap().findings.apply_rescan(ch, title, r.found, r.silences);
            }
            Ok(())
        });
        let mut s = state.lock().unwrap();
        if let Err(e) = result {
            s.log.push(format!("Error: {}", e));
        }
        s.running = false;
    });
    Ok(json!({}))
}

fn start_split(req: SplitReq, state: &Shared) -> Result<Value, String> {
    let output = user_path(&req.output);
    if output.as_os_str().is_empty() {
        return Err("Choose an output folder.".into());
    }
    if output.exists() && !output.is_dir() {
        return Err(format!("'{}' is not a folder.", output.display()));
    }

    let (plan, names) = {
        let mut s = state.lock().unwrap();
        if s.running {
            return Err("A job is already running.".into());
        }
        if s.analyzed.is_empty() {
            return Err("Run an analysis first.".into());
        }
        let (plan, names) = pipeline::prepare_split(&s.analyzed, &req.breaks, &req.meta)?;
        std::fs::create_dir_all(&output)
            .map_err(|e| format!("Failed to create '{}': {}", output.display(), e))?;
        s.running = true;
        s.phase = "split";
        s.stage = "Writing files".into();
        s.done = 0;
        s.total = plan.len();
        s.log.push(format!("Splitting into {}", output.display()));
        (plan, names)
    };

    let state = state.clone();
    thread::spawn(move || {
        pipeline::write_tracks(&plan, &names, &output, &mut |e| apply_event(&state, e));
        let mut s = state.lock().unwrap();
        s.done = s.total;
        s.running = false;
        s.log.push("Done.".into());
    });
    Ok(json!({}))
}
```

Delete the old `fn file_name` at the bottom (the pipeline's is imported). In the `tests` module nothing changes except `AudioFile { .. }` still compiles.

- [ ] **Step 4: Make `main.rs` still compile**

`main.rs` still has its own `--cli` flow using lib functions that all still exist, so nothing to change yet.

Run: `cargo build 2>&1 | tail -20`
Expected: builds. Fix any leftover references to the deleted `Candidate`, `BreakRow` etc. by following the compiler.

- [ ] **Step 5: Run the tests**

Run: `cargo test 2>&1 | tail -15`
Expected: all pass (`gui::tests::clip_rejects_unknown_audio`, `byte_range_parses_requests`, the lib suite).

- [ ] **Step 6: Smoke-test the GUI end to end**

This starts the GUI server (it also opens a browser tab; close it) and drives it with curl exactly as the page does.

```bash
D=$(mktemp -d) && ffmpeg -hide_banner -loglevel error -f lavfi -i "aevalsrc=if(between(t\,4\,6)+between(t\,9\,11)\,0\,sin(2*PI*440*t)):d=15" -y $D/book.wav
cargo build -q && (./target/debug/audiobook-splitter-rust >/dev/null 2>&1 &) ; sleep 1
```

(Until Task 4 the binary starts the GUI when run with no arguments.) Then:

```bash
J='Content-Type: application/json'
curl -s -X POST -H "$J" -d "{\"input\":\"$D/book.wav\",\"min_silence\":1.0,\"noise_db\":-40}" http://127.0.0.1:7878/analyze
sleep 3
curl -s http://127.0.0.1:7878/status | python3 -c 'import json,sys; s=json.load(sys.stdin); print(s["running"], len(s["breaks"]), [b["title"] for b in s["breaks"]], len(s["silences"]), s["log"][-1])'
```

Expected: `False 3 ['Part 1', 'Part 2', 'Part 3'] 3 Analysis complete.`

```bash
curl -s http://127.0.0.1:7878/status | python3 -c '
import json,sys; s=json.load(sys.stdin)
print(json.dumps({"output": sys.argv[1], "breaks": [{k:b[k] for k in ("file","at","title")} for b in s["breaks"]], "meta": s["meta"]}))' $D/out > $D/split.json
curl -s -X POST -H "$J" -d @$D/split.json http://127.0.0.1:7878/split
sleep 3; ls $D/out
curl -s http://127.0.0.1:7878/status | python3 -c 'import json,sys; s=json.load(sys.stdin); print(s["running"], s["done"], s["total"], s["log"][-1])'
pkill -f audiobook-splitter-rust
```

Expected: the folder lists `00 - Part 1.wav`, `01 - Part 2.wav`, `02 - Part 3.wav`; status prints `False 3 3 Done.`

If the EPUB path matters to you, repeat the analyze call once with `"epub":"<a real .epub>"` and confirm `missing` and `score` still appear in `/status` (requires the Whisper model).

- [ ] **Step 7: Commit**

```bash
git add src/gui.rs
git commit -m "Run the GUI on the shared pipeline"
```

---

### Task 4: CLI skeleton, help screen and `gui`

**Files:**
- Modify: `src/main.rs`
- Create: `src/cli/mod.rs`, `src/cli/time.rs`, `src/cli/progress.rs`, `src/cli/prompt.rs`, `tests/common/mod.rs`, `tests/cli.rs`

**Interfaces:**
- Consumes: `pipeline::Event`, `lib::user_path`, `lib::collect_inputs`.
- Produces:
  - `cli::run(args: Vec<String>)` (parses, dispatches, exits 1 on `Err`)
  - `cli::time::clock(secs: f64) -> String` (`h:mm:ss`), `cli::time::parse_time(&str) -> Result<f64, String>`
  - `cli::progress::Reporter` with `new()`, `emit(&mut self, Event)`, `finish(&mut self)`
  - `cli::prompt::require_terminal(command: &str) -> Result<(), String>`, `is_terminal() -> bool`, `ask_input_path() -> Result<PathBuf, String>`, `ask_output_dir() -> Result<PathBuf, String>`, `ask_optional_epub() -> Result<Option<PathBuf>, String>`
  - `tests/common`: `temp_dir(name) -> PathBuf`, `tone_with_gaps(path: &Path, seconds: u32, gaps: &[(u32, u32)])`

- [ ] **Step 1: Write the failing tests**

`tests/common/mod.rs` (integration tests are a separate crate, so the unit-test helpers are repeated here):

```rust
#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abs-cli-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// A WAV `seconds` long: silent during each `(start, end)` gap, a 440 Hz tone elsewhere.
pub fn tone_with_gaps(path: &Path, seconds: u32, gaps: &[(u32, u32)]) {
    let silent = if gaps.is_empty() {
        "0".to_string()
    } else {
        gaps.iter().map(|(a, b)| format!("between(t\\,{}\\,{})", a, b)).collect::<Vec<_>>().join("+")
    };
    let expr = format!("aevalsrc=if({}\\,0\\,sin(2*PI*440*t)):d={}", silent, seconds);
    let made = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &expr, "-y"])
        .arg(path)
        .status()
        .unwrap();
    assert!(made.success());
}
```

`tests/cli.rs`:

```rust
mod common;
use assert_cmd::cargo::cargo_bin_cmd;
use predicates::str::contains;

#[test]
fn no_arguments_prints_help() {
    cargo_bin_cmd!("audiobook-splitter-rust")
        .assert()
        .failure()
        .code(2)
        .stderr(contains("Usage"))
        .stderr(contains("gui"));
}
```

Unit tests in `src/cli/time.rs` (write the whole file now, implementation included, since it is tiny and pure; the failing-first step is the integration test above plus these):

```rust
// Clock times for display and for typing ("1:02:03").

// 3723.4 -> "1:02:03"
pub fn clock(secs: f64) -> String {
    let t = secs.max(0.0).round() as u64;
    format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}

// "1:02:03", "2:03" or plain seconds.
pub fn parse_time(text: &str) -> Result<f64, String> {
    let parts: Result<Vec<f64>, _> = text.trim().split(':').map(|p| p.trim().parse::<f64>()).collect();
    match parts {
        Ok(p) if p.len() <= 3 && p.iter().all(|v| v.is_finite() && *v >= 0.0) => Ok(p.iter().fold(0.0, |sum, v| sum * 60.0 + v)),
        _ => Err("Enter a time like 1:02:03, 2:03 or 95 (seconds).".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats_hours_minutes_seconds() {
        assert_eq!(clock(0.0), "0:00:00");
        assert_eq!(clock(3723.4), "1:02:03");
        assert_eq!(clock(-5.0), "0:00:00");
    }

    #[test]
    fn parse_time_reads_clock_and_seconds() {
        assert_eq!(parse_time("1:02:03"), Ok(3723.0));
        assert_eq!(parse_time(" 2:03 "), Ok(123.0));
        assert_eq!(parse_time("95"), Ok(95.0));
        assert_eq!(parse_time("1:30.5"), Ok(90.5));
        for bad in ["", "abc", "-5", "1:2:3:4", "NaN", "1::2"] {
            assert!(parse_time(bad).is_err(), "{}", bad);
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test cli 2>&1 | tail -10`
Expected: FAIL, because the binary with no arguments launches the GUI instead of printing help (the test hangs or fails; stop it with Ctrl-C if it hangs, and `pkill -f audiobook-splitter-rust`).

- [ ] **Step 3: Write `src/cli/progress.rs`**

```rust
// Draws pipeline events as indicatif bars, or as plain lines when stderr isn't a terminal
// (a log file, `ssh host cmd > out`), where bars would be noise.
use audiobook_splitter_rust::pipeline::Event;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::io::IsTerminal;

pub struct Reporter {
    mp: MultiProgress,
    bar: Option<ProgressBar>,
    tty: bool,
}

impl Reporter {
    pub fn new() -> Self {
        Reporter { mp: MultiProgress::new(), bar: None, tty: std::io::stderr().is_terminal() }
    }

    pub fn emit(&mut self, event: Event) {
        match event {
            Event::Stage { name, total } => {
                self.finish();
                if !self.tty {
                    eprintln!("{} ({} items)", name, total);
                }
                let bar = self.mp.add(ProgressBar::new(total as u64));
                bar.set_style(
                    ProgressStyle::with_template("{msg:<40} [{bar:30.cyan/blue}] {pos}/{len}  {elapsed_precise}  ETA {eta}")
                        .expect("static template")
                        .progress_chars("=> "),
                );
                bar.set_message(name);
                self.bar = Some(bar);
            }
            Event::Tick(done) => {
                if let Some(bar) = &self.bar {
                    bar.set_position(done as u64);
                }
            }
            Event::Log(line) => {
                if self.tty {
                    let _ = self.mp.println(line);
                } else {
                    eprintln!("{}", line);
                }
            }
        }
    }

    // Leaves the last bar on screen at its final position.
    pub fn finish(&mut self) {
        if let Some(bar) = self.bar.take() {
            bar.finish();
        }
    }
}
```

- [ ] **Step 4: Write `src/cli/prompt.rs`**

```rust
// Terminal checks and the few prompts the guided `run` needs.
use audiobook_splitter_rust::{collect_inputs, user_path};
use dialoguer::{theme::ColorfulTheme, Input};
use std::io::IsTerminal;
use std::path::PathBuf;

pub fn is_terminal() -> bool {
    std::io::stdin().is_terminal()
}

pub fn require_terminal(command: &str) -> Result<(), String> {
    if is_terminal() {
        Ok(())
    } else {
        Err(format!(
            "`{}` needs an interactive terminal. Edit the plan file (plain JSON) instead, or run `split` on it as it is.",
            command
        ))
    }
}

fn input(prompt: &str, allow_empty: bool, check: fn(&String) -> Result<(), String>) -> Result<String, String> {
    Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .allow_empty(allow_empty)
        .validate_with(check)
        .interact_text()
        .map_err(|e| e.to_string())
}

pub fn ask_input_path() -> Result<PathBuf, String> {
    require_terminal("run")?;
    let typed = input("Audio file, or a folder of audio files", false, |s| match collect_inputs(&user_path(s)) {
        Ok(files) if !files.is_empty() => Ok(()),
        Ok(_) => Err("No supported audio files in that folder.".to_string()),
        Err(e) => Err(e),
    })?;
    Ok(user_path(&typed))
}

pub fn ask_output_dir() -> Result<PathBuf, String> {
    require_terminal("run")?;
    Ok(user_path(&input("Output folder (created if needed)", false, |_| Ok(()))?))
}

pub fn ask_optional_epub() -> Result<Option<PathBuf>, String> {
    require_terminal("run")?;
    let typed = input("Book EPUB to match chapters with AI (Enter to skip)", true, |s| {
        if s.trim().is_empty() || user_path(s).is_file() { Ok(()) } else { Err("No such file.".to_string()) }
    })?;
    Ok(Some(user_path(&typed)).filter(|p| !p.as_os_str().is_empty()))
}
```

- [ ] **Step 5: Write `src/cli/mod.rs`**

```rust
// Command-line front end: one subcommand per step, joined by a plan file (see plan.rs).
mod progress;
mod prompt;
mod time;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "audiobook-splitter-rust",
    version,
    about = "Split audiobooks into chapters at silences, optionally matched to the book's EPUB",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the web GUI
    Gui,
}

pub fn run(mut args: Vec<String>) {
    // The old `--cli` flag is now `run`.
    if args.get(1).is_some_and(|a| a == "--cli") {
        args[1] = "run".into();
    }
    let result = match Cli::parse_from(args).cmd {
        Cmd::Gui => {
            crate::gui::run();
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
```

- [ ] **Step 6: Replace `src/main.rs`**

```rust
mod cli;
mod gui;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // The GUI re-runs this binary as `--pick <kind>` to show a native file dialog.
    if let Some(i) = args.iter().position(|a| a == "--pick") {
        gui::pick(args.get(i + 1).map_or("file", String::as_str));
        return;
    }
    cli::run(args);
}
```

- [ ] **Step 7: Run the tests**

Run: `cargo test 2>&1 | tail -15`
Expected: all pass, including `cli::time::tests` and `no_arguments_prints_help`. Unused-code warnings for `progress`/`prompt` are expected until later tasks use them.

Run: `cargo run -q -- --help`
Expected: usage text with the `gui` command and `-h/--help`, `-V/--version`.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs src/cli tests
git commit -m "Add the clap CLI skeleton: help by default, gui as a command"
```

---

### Task 5: `analyze` and `split`

**Files:**
- Modify: `src/cli/mod.rs`
- Create: `src/cli/analyze.rs`, `src/cli/split.rs`
- Test: `tests/cli.rs`

**Interfaces:**
- Consumes: Tasks 1-4.
- Produces (in `cli/mod.rs`, used by Tasks 6-8):
  - `cli::Detect { epub: Option<PathBuf>, min_silence: f64, noise_db: f64, threshold: f64 }` (clap args, `fn settings(&self) -> Settings`)
  - `cli::Tags { album, artist, album_artist, genre, year, template: Option<String> }` (clap args, `fn apply(&self, &mut Metadata)`)
  - `cli::check_tools() -> Result<(), String>`
  - `cli::open_plan(&Path) -> Result<Plan, String>` (load + `check_files`)
  - `cli::place(plan: &Plan, file: usize, at: f64) -> String`
  - `cli::analyze::build_plan(input: &Path, detect: &Detect) -> Result<Plan, String>`
  - `cli::analyze::summary(plan: &Plan, plan_path: Option<&Path>)`
  - `cli::split::write_output(plan: &Plan, out: &Path) -> Result<(), String>`

- [ ] **Step 1: Write the failing integration tests**

Append to `tests/cli.rs`:

```rust
use std::fs;

fn bin() -> assert_cmd::Command {
    cargo_bin_cmd!("audiobook-splitter-rust")
}

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

// A 15 s book with two silences, analyzed into `dir/plan.json`.
fn analyzed(name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let dir = common::temp_dir(name);
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let plan = dir.join("plan.json");
    bin().arg("analyze").arg(&wav).args(["--min-silence", "1", "-o"]).arg(&plan).assert().success();
    (dir, wav, plan)
}

#[test]
fn analyze_then_split_without_an_epub() {
    let dir = common::temp_dir("analyze-split");
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let plan = dir.join("plan.json");
    bin()
        .arg("analyze")
        .arg(&wav)
        .args(["--min-silence", "1", "-o"])
        .arg(&plan)
        .assert()
        .success()
        .stdout(contains("Part 3"));
    let out = dir.join("out");
    bin().arg("split").arg(&plan).arg("-o").arg(&out).assert().success().stdout(contains("Wrote 3 file(s)"));
    assert_eq!(names(&out), ["00 - Part 1.wav", "01 - Part 2.wav", "02 - Part 3.wav"]);
}

#[test]
fn split_flags_override_the_template() {
    let (dir, _, plan) = analyzed("template");
    let out = dir.join("out");
    bin().arg("split").arg(&plan).arg("-o").arg(&out).args(["--template", "{n}_{title}", "--album", "Book"]).assert().success();
    assert_eq!(names(&out), ["00_Part 1.wav", "01_Part 2.wav", "02_Part 3.wav"]);
}

#[test]
fn analyze_rejects_bad_input_and_flags() {
    let dir = common::temp_dir("bad-input");
    bin().arg("analyze").arg(dir.join("nothere.wav")).assert().failure().stderr(contains("not a file or folder"));
    let wav = dir.join("a.wav");
    common::tone_with_gaps(&wav, 5, &[]);
    bin().arg("analyze").arg(&wav).args(["--min-silence", "0"]).assert().failure().stderr(contains("Minimum silence"));
    bin().arg("analyze").arg(&wav).args(["--threshold", "2"]).assert().failure().stderr(contains("between 0 and 1"));
    bin().arg("analyze").arg(&wav).args(["--noise-db", "-30", "--min-silence", "1", "-o"]).arg(dir.join("p.json")).assert().success();
}

#[test]
fn a_missing_ffmpeg_is_reported_before_any_work() {
    let dir = common::temp_dir("no-ffmpeg");
    let wav = dir.join("a.wav");
    common::tone_with_gaps(&wav, 5, &[]);
    bin().env("PATH", &dir).arg("analyze").arg(&wav).assert().failure().stderr(contains("was not found on your PATH"));
}

#[test]
fn a_plan_whose_audio_changed_is_rejected() {
    let (dir, wav, plan) = analyzed("changed");
    common::tone_with_gaps(&wav, 40, &[]);
    bin().arg("split").arg(&plan).arg("-o").arg(dir.join("out")).assert().failure().stderr(contains("has changed"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test cli 2>&1 | tail -15`
Expected: the new tests FAIL (`unrecognized subcommand 'analyze'`).

- [ ] **Step 3: Extend `src/cli/mod.rs`**

Replace the file with:

```rust
// Command-line front end: one subcommand per step, joined by a plan file (see plan.rs).
mod analyze;
mod progress;
mod prompt;
mod split;
mod time;

use audiobook_splitter_rust::meta::Metadata;
use audiobook_splitter_rust::pipeline::file_name;
use audiobook_splitter_rust::plan::{Plan, Settings};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Parser)]
#[command(
    name = "audiobook-splitter-rust",
    version,
    about = "Split audiobooks into chapters at silences, optionally matched to the book's EPUB",
    long_about = "Split audiobooks into chapters at silences, optionally matched to the book's EPUB.\n\n\
        Work in steps joined by a plan file: `analyze` writes it, `edit` (and `rescan`) change it, \
        `split` writes the audio files from it. `run` does all three in one go.",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Detect silences (and match EPUB chapters) and write a plan file
    Analyze(analyze::Args),
    /// Write the tagged audio files described by a plan file
    Split(split::Args),
    /// Start the web GUI
    Gui,
}

// Shared by `analyze` and `run`.
#[derive(clap::Args)]
pub struct Detect {
    /// The book's EPUB: split only where the narration matches a chapter start
    #[arg(long)]
    pub epub: Option<PathBuf>,
    /// Shortest silence, in seconds, that can be a break (use 1-2 with an EPUB)
    #[arg(long, default_value_t = 2.0)]
    pub min_silence: f64,
    /// Level in dB below which audio counts as silence; less negative finds more
    #[arg(long, default_value_t = -40.0, allow_negative_numbers = true)]
    pub noise_db: f64,
    /// Minimum EPUB match score, 0 to 1
    #[arg(long, default_value_t = 0.4)]
    pub threshold: f64,
}

impl Detect {
    pub fn settings(&self) -> Settings {
        Settings { min_silence: self.min_silence, noise_db: self.noise_db, threshold: self.threshold }
    }
}

// Shared by `split` and `run`. Anything left out keeps the plan's value.
#[derive(clap::Args)]
pub struct Tags {
    /// Album tag
    #[arg(long)]
    pub album: Option<String>,
    /// Artist tag
    #[arg(long)]
    pub artist: Option<String>,
    /// Album artist tag
    #[arg(long)]
    pub album_artist: Option<String>,
    /// Genre tag
    #[arg(long)]
    pub genre: Option<String>,
    /// Year tag
    #[arg(long)]
    pub year: Option<String>,
    /// File naming, with {n} (counts from 00), {title} and {album}
    #[arg(long)]
    pub template: Option<String>,
}

impl Tags {
    pub fn apply(&self, meta: &mut Metadata) {
        fn set(field: &mut String, value: &Option<String>) {
            if let Some(v) = value {
                *field = v.clone();
            }
        }
        set(&mut meta.album, &self.album);
        set(&mut meta.artist, &self.artist);
        set(&mut meta.album_artist, &self.album_artist);
        set(&mut meta.genre, &self.genre);
        set(&mut meta.year, &self.year);
        set(&mut meta.template, &self.template);
    }
}

pub fn check_tools() -> Result<(), String> {
    for tool in ["ffmpeg", "ffprobe"] {
        Command::new(tool)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| {
                format!(
                    "{} was not found on your PATH. Install FFmpeg (macOS: brew install ffmpeg; Debian/Ubuntu: sudo apt install ffmpeg; Windows: winget install Gyan.FFmpeg).",
                    tool
                )
            })?;
    }
    Ok(())
}

// Loads a plan and checks its audio files are still the ones it was made from.
pub fn open_plan(path: &Path) -> Result<Plan, String> {
    let plan = Plan::load(path)?;
    plan.check_files()?;
    Ok(plan)
}

// "0:05:12", prefixed with the file name when the book has several files.
pub fn place(plan: &Plan, file: usize, at: f64) -> String {
    let when = time::clock(at);
    if plan.files.len() > 1 { format!("{} @ {}", file_name(&plan.files[file].path), when) } else { when }
}

pub fn run(mut args: Vec<String>) {
    // The old `--cli` flag is now `run`.
    if args.get(1).is_some_and(|a| a == "--cli") {
        args[1] = "run".into();
    }
    let result = match Cli::parse_from(args).cmd {
        Cmd::Analyze(a) => analyze::run(a),
        Cmd::Split(a) => split::run(a),
        Cmd::Gui => {
            crate::gui::run();
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
```

(`--cli` rewrites to `run`, which exists only after Task 8; until then it prints clap's unknown-subcommand error, which is fine.)

- [ ] **Step 4: Write `src/cli/analyze.rs`**

```rust
// `analyze`: detect silences, optionally match the EPUB, write the plan.
use super::progress::Reporter;
use super::{check_tools, place, Detect};
use audiobook_splitter_rust::collect_inputs;
use audiobook_splitter_rust::pipeline::{analyze, default_metadata, open_epub};
use audiobook_splitter_rust::plan::Plan;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct Args {
    /// An audio file, or a folder of audio files (treated as one book, in file-name order)
    pub input: PathBuf,
    /// Where to write the plan
    #[arg(short, long, default_value = "plan.json")]
    pub out: PathBuf,
    #[command(flatten)]
    pub detect: Detect,
}

pub fn run(args: Args) -> Result<(), String> {
    let plan = build_plan(&args.input, &args.detect)?;
    plan.save(&args.out)?;
    summary(&plan, Some(&args.out));
    println!("\nPlan written to {}.", args.out.display());
    Ok(())
}

// Checks everything cheap first, then runs the analysis with progress bars.
pub fn build_plan(input: &Path, detect: &Detect) -> Result<Plan, String> {
    check_tools()?;
    let settings = detect.settings();
    settings.validate()?;
    let inputs = collect_inputs(input)?;
    if inputs.is_empty() {
        return Err(format!("No supported audio files found in '{}'.", input.display()));
    }
    let (epub, meta) = match &detect.epub {
        Some(path) => {
            let (epub, info) = open_epub(path)?;
            (Some(epub), default_metadata(Some(info)))
        }
        None => (None, default_metadata(None)),
    };

    let mut reporter = Reporter::new();
    let result = analyze(&inputs, &settings, epub.as_ref(), &mut |e| reporter.emit(e));
    reporter.finish();
    let analysis = result?;
    if analysis.files.is_empty() {
        return Err("None of the audio files could be analyzed.".into());
    }
    Ok(Plan::new(analysis.files, settings, detect.epub.clone(), analysis.findings, meta))
}

// The chapter list, and what to run next.
pub fn summary(plan: &Plan, plan_path: Option<&Path>) {
    let f = &plan.findings;
    println!("\n{:>3}  {:<28} {:<40} {}", "#", "Where", "Title", "Match");
    for (i, b) in f.breaks.iter().enumerate() {
        let score = b.score.map_or(String::new(), |s| format!("{:.2}", s));
        println!("{:>3}  {:<28} {:<40} {}", i + 1, place(plan, b.file, b.at), b.title, score);
    }
    println!("\n{} chapter(s).", f.breaks.len());
    if !f.missing.is_empty() {
        let names: Vec<String> = f.missing.iter().map(|m| format!("{}. {}", m.chapter + 1, m.title)).collect();
        println!("No break found for: {}", names.join(", "));
        if let Some(p) = plan_path {
            println!("Try `rescan {}`, or a smaller --min-silence or --threshold.", p.display());
        }
    }
}
```

- [ ] **Step 5: Write `src/cli/split.rs`**

```rust
// `split`: write the tagged audio files a plan describes.
use super::progress::Reporter;
use super::{check_tools, open_plan, Tags};
use audiobook_splitter_rust::pipeline::{prepare_split, write_tracks};
use audiobook_splitter_rust::plan::Plan;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct Args {
    /// Plan file made by `analyze`
    pub plan: PathBuf,
    /// Folder for the output files (created if needed)
    #[arg(short, long)]
    pub out: PathBuf,
    #[command(flatten)]
    pub tags: Tags,
}

pub fn run(args: Args) -> Result<(), String> {
    check_tools()?;
    let mut plan = open_plan(&args.plan)?;
    args.tags.apply(&mut plan.metadata);
    write_output(&plan, &args.out)
}

pub fn write_output(plan: &Plan, out: &Path) -> Result<(), String> {
    if out.exists() && !out.is_dir() {
        return Err(format!("'{}' is not a folder.", out.display()));
    }
    let (segments, names) = prepare_split(&plan.files, &plan.findings.to_breaks(), &plan.metadata)?;
    std::fs::create_dir_all(out).map_err(|e| format!("Failed to create '{}': {}", out.display(), e))?;

    let mut reporter = Reporter::new();
    let report = write_tracks(&segments, &names, out, &mut |e| reporter.emit(e));
    reporter.finish();
    println!("Wrote {} file(s) to {}.", report.written.len(), out.display());
    if report.failed > 0 {
        return Err(format!("{} file(s) could not be written.", report.failed));
    }
    Ok(())
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test 2>&1 | tail -20`
Expected: all pass. If `--noise-db -30` is mis-parsed by clap, `allow_negative_numbers = true` on the arg is the fix; the test covers it.

- [ ] **Step 7: Commit**

```bash
git add src/cli tests/cli.rs
git commit -m "Add analyze and split commands"
```

---

### Task 6: The chapter and tag editor (`edit`)

**Files:**
- Modify: `src/cli/mod.rs` (add `mod edit;`, `Edit(edit::Args)` variant and dispatch arm)
- Create: `src/cli/edit.rs`
- Test: unit tests in `src/cli/edit.rs`, one in `tests/cli.rs`

**Interfaces:**
- Consumes: `plan::{Plan, PlanBreak, PlanSilence}`, `cli::{open_plan, place}`, `cli::time::{clock, parse_time}`, `cli::prompt::require_terminal`.
- Produces:
  - `edit::is_fixed(&PlanBreak) -> bool`
  - `edit::add(plan: &mut Plan, file: usize, at: f64, title: &str) -> Result<(), String>`
  - `edit::rename(plan: &mut Plan, i: usize, title: &str) -> Result<(), String>`
  - `edit::remove(plan: &mut Plan, i: usize) -> Result<(), String>`
  - `edit::neighbour(plan: &Plan, i: usize, dir: i32) -> Option<(usize, f64)>`
  - `edit::move_break(plan: &mut Plan, i: usize, dir: i32) -> Result<(), String>`
  - `edit::unused_silences(plan: &Plan) -> Vec<(usize, f64)>`
  - `edit::edit_plan(plan: &mut Plan) -> Result<bool, String>` (true = save)

The rules mirror the GUI editor: the first chapter (start of the book) can't be moved or removed; a move goes to the previous or next silence and may not cross a neighbouring break; adding at an occupied position is an error.

- [ ] **Step 1: Write the failing unit tests**

Create `src/cli/edit.rs` containing only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use audiobook_splitter_rust::chapters::AudioFile;
    use audiobook_splitter_rust::meta::Metadata;
    use audiobook_splitter_rust::plan::{Findings, PlanSilence, Settings};

    // One 100 s file, chapters at 0, 30 and 60 s, silences at 0, 10, 20, 30, 45, 60 and 80 s.
    fn plan() -> Plan {
        let silence = |at: f64| PlanSilence { file: 0, at, before: String::new(), after: String::new() };
        let brk = |at: f64, title: &str| PlanBreak { file: 0, at, title: title.into(), score: None };
        Plan::new(
            vec![AudioFile { path: "a.mp3".into(), silences: vec![], duration: 100.0 }],
            Settings::default(),
            None,
            Findings {
                breaks: vec![brk(0.0, "One"), brk(30.0, "Two"), brk(60.0, "Three")],
                silences: [0.0, 10.0, 20.0, 30.0, 45.0, 60.0, 80.0].map(silence).to_vec(),
                ..Default::default()
            },
            Metadata::default(),
        )
    }

    fn titles(p: &Plan) -> Vec<&str> {
        p.findings.breaks.iter().map(|b| b.title.as_str()).collect()
    }

    #[test]
    fn add_inserts_in_order_and_rejects_bad_positions() {
        let mut p = plan();
        add(&mut p, 0, 45.0, "New").unwrap();
        assert_eq!(titles(&p), ["One", "Two", "New", "Three"]);
        assert!(add(&mut p, 0, 45.0, "Again").unwrap_err().contains("already a chapter"));
        assert!(add(&mut p, 0, 100.0, "Late").is_err());
        assert!(add(&mut p, 0, -1.0, "Early").is_err());
        assert!(add(&mut p, 3, 5.0, "Nofile").is_err());
    }

    #[test]
    fn rename_trims_and_rejects_empty() {
        let mut p = plan();
        rename(&mut p, 1, "  Storm ").unwrap();
        assert_eq!(titles(&p)[1], "Storm");
        assert!(rename(&mut p, 1, "  ").is_err());
        assert!(rename(&mut p, 9, "x").is_err());
    }

    #[test]
    fn the_first_chapter_cannot_be_removed_or_moved() {
        let mut p = plan();
        assert!(remove(&mut p, 0).is_err());
        assert!(move_break(&mut p, 0, 1).is_err());
        remove(&mut p, 1).unwrap();
        assert_eq!(titles(&p), ["One", "Three"]);
        assert!(remove(&mut p, 5).is_err());
    }

    #[test]
    fn moves_go_to_the_adjacent_silence_but_not_past_a_neighbour() {
        let mut p = plan();
        // "Two" is at 30; the silence before it is 20, after it 45.
        assert_eq!(neighbour(&p, 1, -1), Some((0, 20.0)));
        assert_eq!(neighbour(&p, 1, 1), Some((0, 45.0)));
        move_break(&mut p, 1, 1).unwrap();
        assert_eq!(p.findings.breaks[1].at, 45.0);
        // Next silence after 45 is 60, which is "Three": not allowed to land on it.
        assert_eq!(neighbour(&p, 1, 1), None);
        assert!(move_break(&mut p, 1, 1).unwrap_err().contains("no silence"));
        move_break(&mut p, 1, -1).unwrap();
        assert_eq!(p.findings.breaks[1].at, 30.0);
    }

    #[test]
    fn moving_clears_the_match_score() {
        let mut p = plan();
        p.findings.breaks[1].score = Some(1.2);
        move_break(&mut p, 1, -1).unwrap();
        assert_eq!(p.findings.breaks[1].score, None);
    }

    #[test]
    fn unused_silences_skip_those_that_start_a_chapter() {
        let p = plan();
        let at: Vec<f64> = unused_silences(&p).into_iter().map(|(_, at)| at).collect();
        assert_eq!(at, [10.0, 20.0, 45.0, 80.0]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test cli::edit 2>&1 | tail -15`
Expected: compile errors (`add`, `rename`, ... not found; `edit` not yet a module, so first add `mod edit;` to `cli/mod.rs`).

- [ ] **Step 3: Write the pure edits**

Add `mod edit;` to the `mod` list in `src/cli/mod.rs`. Put this above the test module in `src/cli/edit.rs`:

```rust
// `edit`: change a plan's chapters and tags by hand. The functions up top are plain edits
// on the plan (unit-tested); the menu below only chooses which one to call.
use super::time::{clock, parse_time};
use super::{open_plan, place, prompt};
use audiobook_splitter_rust::plan::{Plan, PlanBreak};
use dialoguer::{theme::ColorfulTheme, Input, Select};
use std::cmp::Ordering;
use std::path::PathBuf;

type Pos = (usize, f64); // (file index, seconds into that file)

fn pos(b: &PlanBreak) -> Pos {
    (b.file, b.at)
}

fn order(a: Pos, b: Pos) -> Ordering {
    a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))
}

fn same(a: Pos, b: Pos) -> bool {
    a.0 == b.0 && (a.1 - b.1).abs() < 0.001
}

// The first chapter starts the book; it always exists.
pub fn is_fixed(b: &PlanBreak) -> bool {
    b.file == 0 && b.at <= 0.01
}

pub fn add(plan: &mut Plan, file: usize, at: f64, title: &str) -> Result<(), String> {
    let duration = plan.files.get(file).ok_or("There is no such audio file.")?.duration;
    if !(at >= 0.0 && at < duration) {
        return Err(format!("Enter a time between 0:00:00 and {}.", clock(duration)));
    }
    let breaks = &mut plan.findings.breaks;
    if breaks.iter().any(|b| same(pos(b), (file, at))) {
        return Err("There is already a chapter there.".into());
    }
    breaks.push(PlanBreak { file, at, title: title.into(), score: None });
    breaks.sort_by(|a, b| order(pos(a), pos(b)));
    Ok(())
}

pub fn rename(plan: &mut Plan, i: usize, title: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("A chapter needs a title.".into());
    }
    plan.findings.breaks.get_mut(i).ok_or("No such chapter.")?.title = title.into();
    Ok(())
}

pub fn remove(plan: &mut Plan, i: usize) -> Result<(), String> {
    if is_fixed(plan.findings.breaks.get(i).ok_or("No such chapter.")?) {
        return Err("The first chapter starts the book and can't be removed.".into());
    }
    plan.findings.breaks.remove(i);
    Ok(())
}

// The silence break `i` would move to: the previous (`dir` < 0) or next one, staying
// between its neighbouring breaks.
pub fn neighbour(plan: &Plan, i: usize, dir: i32) -> Option<Pos> {
    let breaks = &plan.findings.breaks;
    let here = pos(breaks.get(i)?);
    let prev = i.checked_sub(1).map(|p| pos(&breaks[p]));
    let next = breaks.get(i + 1).map(pos);
    let ok = |s: Pos| prev.is_none_or(|p| order(s, p).is_gt()) && next.is_none_or(|n| order(s, n).is_lt());
    let silences = plan.findings.silences.iter().map(|s| (s.file, s.at));
    if dir < 0 {
        silences.filter(|&s| order(s, here).is_lt() && ok(s)).last()
    } else {
        silences.into_iter().find(|&s| order(s, here).is_gt() && ok(s))
    }
}

pub fn move_break(plan: &mut Plan, i: usize, dir: i32) -> Result<(), String> {
    if is_fixed(plan.findings.breaks.get(i).ok_or("No such chapter.")?) {
        return Err("The first chapter starts the book and can't be moved.".into());
    }
    let (file, at) = neighbour(plan, i, dir).ok_or("There is no silence to move to.")?;
    let b = &mut plan.findings.breaks[i];
    (b.file, b.at, b.score) = (file, at, None);
    Ok(())
}

// Silences the analysis skipped: those where no chapter starts.
pub fn unused_silences(plan: &Plan) -> Vec<Pos> {
    plan.findings
        .silences
        .iter()
        .map(|s| (s.file, s.at))
        .filter(|&s| !plan.findings.breaks.iter().any(|b| same(pos(b), s)))
        .collect()
}

// What Whisper heard either side of a break, if it transcribed there.
fn heard(plan: &Plan, i: usize) -> Option<(&str, &str)> {
    let at = pos(plan.findings.breaks.get(i)?);
    let s = plan.findings.silences.iter().find(|s| same((s.file, s.at), at))?;
    Some((&s.before, &s.after)).filter(|(b, a)| !(b.is_empty() && a.is_empty()))
}
```

- [ ] **Step 4: Run the unit tests**

Run: `cargo test cli::edit 2>&1 | tail -15`
Expected: the 6 edit tests pass. (Warnings for the unused menu imports are fine until the next step.)

- [ ] **Step 5: Write the menu and the command**

Append to `src/cli/edit.rs` above the test module:

```rust
fn io<T>(r: std::io::Result<T>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}

fn pick(prompt: &str, items: &[String]) -> Result<Option<usize>, String> {
    io(Select::with_theme(&ColorfulTheme::default()).with_prompt(prompt).items(items).default(0).interact_opt())
}

fn ask(prompt: &str, initial: &str, allow_empty: bool) -> Result<String, String> {
    io(Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .with_initial_text(initial)
        .allow_empty(allow_empty)
        .interact_text())
}

// A mistake or refusal is shown and the menu carries on; only a broken terminal ends it.
fn report(result: Result<(), String>) {
    if let Err(e) = result {
        println!("{}", e);
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// Runs the editor on `plan`. Returns whether to save it.
pub fn edit_plan(plan: &mut Plan) -> Result<bool, String> {
    let original = (plan.findings.breaks.clone(), plan.metadata.clone());
    let menu = strings(&[
        "Review and edit chapters",
        "Add a chapter at a time",
        "Add a chapter at a silence the analysis skipped",
        "Edit book tags and file naming",
        "Discard all changes",
        "Save and exit",
        "Exit without saving",
    ]);
    loop {
        let missing = plan.findings.missing.len();
        println!("\n{} chapter(s){}", plan.findings.breaks.len(), if missing > 0 { format!(", {} not found", missing) } else { String::new() });
        match pick("What next?", &menu)? {
            Some(0) => chapters_menu(plan)?,
            Some(1) => report(add_at_time(plan)),
            Some(2) => report(add_at_silence(plan)),
            Some(3) => tags_menu(plan)?,
            Some(4) => {
                (plan.findings.breaks, plan.metadata) = original.clone();
                println!("Changes discarded.");
            }
            Some(5) => return Ok(true),
            Some(6) => return Ok(false),
            _ => {} // Esc: stay, so a stray key can't lose the edits.
        }
    }
}

fn chapters_menu(plan: &mut Plan) -> Result<(), String> {
    loop {
        let mut items: Vec<String> = plan
            .findings
            .breaks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let score = b.score.map_or(String::new(), |s| format!("  (match {:.2})", s));
                format!("{:>3}. {}  {}{}", i + 1, place(plan, b.file, b.at), b.title, score)
            })
            .collect();
        items.push("Back".into());
        match pick("Choose a chapter", &items)? {
            Some(i) if i < plan.findings.breaks.len() => chapter_menu(plan, i)?,
            _ => return Ok(()),
        }
    }
}

fn chapter_menu(plan: &mut Plan, i: usize) -> Result<(), String> {
    let menu = strings(&["Rename", "Move to the previous silence", "Move to the next silence", "Show what Whisper heard", "Remove", "Back"]);
    loop {
        let title = plan.findings.breaks[i].title.clone();
        let result = match pick(&title, &menu)? {
            Some(0) => rename(plan, i, &ask("Title", &title, false)?),
            Some(1) => move_break(plan, i, -1),
            Some(2) => move_break(plan, i, 1),
            Some(3) => {
                match heard(plan, i) {
                    Some((before, after)) => println!("  heard before: {}\n  heard after:  {}", before, after),
                    None => println!("  Nothing was transcribed here (no EPUB, or the break was added by hand)."),
                }
                Ok(())
            }
            Some(4) => match remove(plan, i) {
                Ok(()) => return Ok(()),
                err => err,
            },
            _ => return Ok(()),
        };
        report(result);
        if let Some(b) = plan.findings.breaks.get(i) {
            println!("  now at {}", place(plan, b.file, b.at));
        }
    }
}

fn add_at_time(plan: &mut Plan) -> Result<(), String> {
    let file = if plan.files.len() > 1 {
        let names: Vec<String> = plan.files.iter().map(|f| audiobook_splitter_rust::pipeline::file_name(&f.path)).collect();
        match pick("Which file?", &names)? {
            Some(i) => i,
            None => return Ok(()),
        }
    } else {
        0
    };
    let at = parse_time(&ask("Start time (h:mm:ss)", "", false)?)?;
    let title = ask("Title", "New chapter", false)?;
    add(plan, file, at, &title)
}

fn add_at_silence(plan: &mut Plan) -> Result<(), String> {
    let spots = unused_silences(plan);
    if spots.is_empty() {
        return Err("Every silence already starts a chapter.".into());
    }
    let items: Vec<String> = spots
        .iter()
        .map(|&(file, at)| {
            let after = plan.findings.silences.iter().find(|s| same((s.file, s.at), (file, at))).map_or("", |s| s.after.as_str());
            let preview: String = after.chars().take(50).collect();
            format!("{}  {}", place(plan, file, at), preview)
        })
        .collect();
    let Some(i) = pick("Start a chapter at", &items)? else { return Ok(()) };
    let title = ask("Title", "New chapter", false)?;
    add(plan, spots[i].0, spots[i].1, &title)
}

fn tags_menu(plan: &mut Plan) -> Result<(), String> {
    println!("Leave a tag empty to keep the source file's own tag.");
    let m = &mut plan.metadata;
    m.album = ask("Album", &m.album, true)?;
    m.artist = ask("Artist", &m.artist, true)?;
    m.album_artist = ask("Album artist", &m.album_artist, true)?;
    m.genre = ask("Genre", &m.genre, true)?;
    m.year = ask("Year", &m.year, true)?;
    m.template = ask("File naming: {n} (from 00), {title}, {album}", &m.template, false)?;
    Ok(())
}

#[derive(clap::Args)]
pub struct Args {
    /// Plan file made by `analyze`; edited in place
    pub plan: PathBuf,
}

pub fn run(args: Args) -> Result<(), String> {
    prompt::require_terminal("edit")?;
    let mut plan = open_plan(&args.plan)?;
    if edit_plan(&mut plan)? {
        plan.save(&args.plan)?;
        println!("Saved {}.", args.plan.display());
    } else {
        println!("Nothing saved.");
    }
    Ok(())
}
```

In `src/cli/mod.rs`, add the variant and arm:

```rust
    /// Review and change a plan's chapters, titles and tags (interactive)
    Edit(edit::Args),
```

```rust
        Cmd::Edit(a) => edit::run(a),
```

- [ ] **Step 6: Integration test for the non-terminal case**

Append to `tests/cli.rs`:

```rust
#[test]
fn edit_needs_a_terminal() {
    let (_, _, plan) = analyzed("edit-tty");
    bin().arg("edit").arg(&plan).assert().failure().stderr(contains("interactive terminal"));
}
```

- [ ] **Step 7: Run everything, then try the editor by hand**

Run: `cargo test 2>&1 | tail -15`
Expected: all pass.

Manual check (needs a real terminal; use `! ` in the Claude prompt or a separate shell):

```bash
D=$(mktemp -d) && ffmpeg -hide_banner -loglevel error -f lavfi -i "aevalsrc=if(between(t\,4\,6)+between(t\,9\,11)\,0\,sin(2*PI*440*t)):d=15" -y $D/book.wav
cargo run -q -- analyze $D/book.wav --min-silence 1 -o $D/plan.json
cargo run -q -- edit $D/plan.json
```

Rename "Part 2" to "Storm", move "Part 3" to the previous silence (it must refuse: it would cross "Part 2"... it won't, the previous silence at 5 s is taken, so the menu says there is no silence to move to), save, then `cargo run -q -- split $D/plan.json -o $D/out` and confirm `01 - Storm.wav` exists.

- [ ] **Step 8: Commit**

```bash
git add src/cli tests/cli.rs
git commit -m "Add the interactive chapter and tag editor"
```

---

### Task 7: `rescan` and `preview`

**Files:**
- Modify: `src/cli/mod.rs`
- Create: `src/cli/rescan.rs`, `src/cli/preview.rs`
- Test: `tests/cli.rs`

**Interfaces:**
- Consumes: `pipeline::{open_epub, rescan_one, chapter_title, PREVIEW_SECONDS}`, `lib::preview_clip`, `transcribe::Transcriber`, `Plan::sort_breaks`, `cli::analyze::summary`.
- Produces: `rescan::Args`, `rescan::run`, `preview::Args`, `preview::run`.

Chapter numbers on the command line are 1-based, as in the summary's "No break found for: 3. The Storm". `preview` takes the break number from the `#` column. A real Whisper re-scan can't run in CI (no model), so the automated tests cover the failure paths and `preview`; the re-scan itself is verified by hand in Step 6.

- [ ] **Step 1: Write the failing tests**

Append to `tests/cli.rs`:

```rust
#[test]
fn rescan_needs_a_plan_made_with_an_epub() {
    let (_, _, plan) = analyzed("rescan-no-epub");
    bin().arg("rescan").arg(&plan).assert().failure().stderr(contains("without an EPUB"));
}

#[test]
fn preview_writes_a_clip_from_the_chosen_break() {
    let (dir, _, plan) = analyzed("preview");
    let clip = dir.join("clip.mp3");
    bin().arg("preview").arg(&plan).arg("2").arg("-o").arg(&clip).assert().success().stdout(contains("clip.mp3"));
    assert!(fs::read(&clip).unwrap().starts_with(b"ID3"));
    bin().arg("preview").arg(&plan).arg("9").arg("-o").arg(&clip).assert().failure().stderr(contains("no chapter 9"));
    bin().arg("preview").arg(&plan).arg("0").arg("-o").arg(&clip).assert().failure().stderr(contains("no chapter 0"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test cli rescan_needs preview_writes 2>&1 | tail -10`
Expected: FAIL (`unrecognized subcommand`).

- [ ] **Step 3: Write `src/cli/rescan.rs`**

```rust
// `rescan`: look again for the chapters the match missed, with looser silence settings.
use super::analyze::summary;
use super::progress::Reporter;
use super::{check_tools, open_plan};
use audiobook_splitter_rust::pipeline::{chapter_title, open_epub, rescan_one};
use audiobook_splitter_rust::transcribe::Transcriber;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// Plan file made by `analyze --epub`; updated in place
    pub plan: PathBuf,
    /// Only this missing chapter (its number in the "No break found" list); repeatable
    #[arg(long = "chapter", value_name = "N")]
    pub chapters: Vec<usize>,
}

pub fn run(args: Args) -> Result<(), String> {
    check_tools()?;
    let mut plan = open_plan(&args.plan)?;
    let epub_path = plan.epub.clone().ok_or("This plan was made without an EPUB, so there is nothing to re-scan against.")?;
    let (epub, _) = open_epub(&epub_path)?;

    let missing: Vec<usize> = plan.findings.missing.iter().map(|m| m.chapter).collect();
    let mut wanted = if args.chapters.is_empty() {
        missing.clone()
    } else {
        let mut chosen = Vec::new();
        for n in &args.chapters {
            let ch = n.checked_sub(1).filter(|c| missing.contains(c)).ok_or(format!("Chapter {} isn't missing from this plan.", n))?;
            chosen.push(ch);
        }
        chosen
    };
    if wanted.is_empty() {
        println!("Nothing is missing.");
        return Ok(());
    }
    wanted.sort();
    wanted.dedup();

    let transcriber = Transcriber::load(&epub.model)?;
    let mut reporter = Reporter::new();
    let mut outcome = Ok(());
    for ch in wanted {
        let found = plan.findings.found.clone();
        match rescan_one(ch, &plan.files, &found, &plan.settings, &epub, &transcriber, &mut |e| reporter.emit(e)) {
            Ok(r) => plan.findings.apply_rescan(ch, chapter_title(&epub.chapters, ch), r.found, r.silences),
            Err(e) => {
                outcome = Err(e);
                break;
            }
        }
    }
    reporter.finish();
    // Keep what was found even if a later chapter failed.
    plan.sort_breaks();
    plan.save(&args.plan)?;
    outcome?;
    summary(&plan, Some(&args.plan));
    Ok(())
}
```

- [ ] **Step 4: Write `src/cli/preview.rs`**

```rust
// `preview`: cut the audio after a break to a file, to check it by ear.
use super::{open_plan, place};
use audiobook_splitter_rust::pipeline::PREVIEW_SECONDS;
use audiobook_splitter_rust::preview_clip;
use std::path::PathBuf;
use std::process::Command;

#[derive(clap::Args)]
pub struct Args {
    /// Plan file made by `analyze`
    pub plan: PathBuf,
    /// Chapter number, from the `#` column of the chapter list
    pub n: usize,
    /// Where to write the clip (default: break-N.mp3 in the current folder)
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Also play it with ffplay (needs a local audio device)
    #[arg(long)]
    pub play: bool,
}

pub fn run(args: Args) -> Result<(), String> {
    super::check_tools()?;
    let plan = open_plan(&args.plan)?;
    let b = args
        .n
        .checked_sub(1)
        .and_then(|i| plan.findings.breaks.get(i))
        .ok_or(format!("There is no chapter {} in this plan.", args.n))?;
    let mp3 = preview_clip(&plan.files[b.file].path, b.at, PREVIEW_SECONDS)?;
    let out = args.out.unwrap_or_else(|| PathBuf::from(format!("break-{}.mp3", args.n)));
    std::fs::write(&out, mp3).map_err(|e| format!("Failed to write '{}': {}", out.display(), e))?;
    println!("Wrote {} s from \"{}\" ({}) to {}.", PREVIEW_SECONDS, b.title, place(&plan, b.file, b.at), out.display());

    // ponytail: only plays on request; detecting a local audio device isn't portable.
    if args.play {
        Command::new("ffplay")
            .args(["-nodisp", "-autoexit", "-loglevel", "quiet"])
            .arg(&out)
            .status()
            .map_err(|_| "ffplay was not found on your PATH (it ships with FFmpeg). Play the file yourself, e.g. after `scp`.".to_string())?;
    }
    Ok(())
}
```

- [ ] **Step 5: Wire both into `src/cli/mod.rs`**

Add `mod preview;` and `mod rescan;` to the module list, and:

```rust
    /// Look again for chapters the EPUB match missed
    Rescan(rescan::Args),
    /// Cut the audio after a chapter break to a file, to check it by ear
    Preview(preview::Args),
```

```rust
        Cmd::Rescan(a) => rescan::run(a),
        Cmd::Preview(a) => preview::run(a),
```

Note: the "no chapter 9" test string is `There is no chapter 9`, which contains `no chapter 9`.

- [ ] **Step 6: Run the tests, then verify the real re-scan by hand**

Run: `cargo test 2>&1 | tail -15`
Expected: all pass.

Manual (needs an EPUB, its audio and the model): analyze with a deliberately strict `--min-silence 6` so a chapter is missed, then `rescan plan.json`; the summary should list it as found or "no match", and `plan.json` should update. Compare with the GUI's "Re-scan" on the same inputs.

- [ ] **Step 7: Commit**

```bash
git add src/cli tests/cli.rs
git commit -m "Add rescan and preview commands"
```

---

### Task 8: `run`, legacy `--cli`, docs

**Files:**
- Modify: `src/cli/mod.rs`, `README.md`, `Makefile`
- Create: `src/cli/run.rs`
- Test: `tests/cli.rs`

**Interfaces:**
- Consumes: `analyze::{build_plan, summary}`, `edit::edit_plan`, `split::write_output`, `prompt::*`, `Detect`, `Tags`.
- Produces: `run::Args`, `run::run`.

`run` chains analyze, edit and split. Input and output are prompted for when omitted (needs a terminal); the EPUB is only prompted for when the input was also omitted (the guided, old `--cli` style session). The editor opens only on a terminal without `--yes`. The plan is saved next to the output (`<out>/plan.json`) unless `--plan` is given, so an interrupted split can be resumed with `split`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/cli.rs`:

```rust
#[test]
fn run_chains_analyze_and_split() {
    let dir = common::temp_dir("run");
    let wav = dir.join("book.wav");
    common::tone_with_gaps(&wav, 15, &[(4, 6), (9, 11)]);
    let out = dir.join("out");
    bin().arg("run").arg(&wav).arg("-o").arg(&out).args(["--min-silence", "1", "--yes"]).assert().success().stdout(contains("Wrote 3 file(s)"));
    assert_eq!(names(&out), ["00 - Part 1.wav", "01 - Part 2.wav", "02 - Part 3.wav", "plan.json"]);
}

#[test]
fn run_without_a_terminal_cannot_prompt() {
    bin().arg("run").assert().failure().stderr(contains("interactive terminal"));
}

#[test]
fn the_old_cli_flag_still_works_as_run() {
    bin().arg("--cli").assert().failure().stderr(contains("interactive terminal"));
}
```

(The plan file in the output folder is intentional: it is what lets a failed or interrupted split be resumed.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test cli run_ the_old 2>&1 | tail -10`
Expected: FAIL (`unrecognized subcommand 'run'`).

- [ ] **Step 3: Write `src/cli/run.rs`**

```rust
// `run`: analyze, review and split in one go. Missing paths are prompted for.
use super::analyze::{build_plan, summary};
use super::edit::edit_plan;
use super::split::write_output;
use super::{prompt, Detect, Tags};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// An audio file, or a folder of audio files (asked for if left out)
    pub input: Option<PathBuf>,
    /// Folder for the output files (asked for if left out)
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Where to save the plan (default: plan.json in the output folder)
    #[arg(long)]
    pub plan: Option<PathBuf>,
    /// Don't open the chapter editor; split what the analysis found
    #[arg(short, long)]
    pub yes: bool,
    #[command(flatten)]
    pub detect: Detect,
    #[command(flatten)]
    pub tags: Tags,
}

pub fn run(mut args: Args) -> Result<(), String> {
    // With no input given this is a guided session, like the old interactive mode.
    let guided = args.input.is_none();
    let input = match args.input.take() {
        Some(p) => p,
        None => prompt::ask_input_path()?,
    };
    let out = match args.out.take() {
        Some(p) => p,
        None => prompt::ask_output_dir()?,
    };
    if guided && args.detect.epub.is_none() {
        args.detect.epub = prompt::ask_optional_epub()?;
    }

    let mut plan = build_plan(&input, &args.detect)?;
    args.tags.apply(&mut plan.metadata);
    let plan_path = args.plan.unwrap_or_else(|| out.join("plan.json"));
    plan.save(&plan_path)?;
    summary(&plan, Some(&plan_path));

    if prompt::is_terminal() && !args.yes {
        if !edit_plan(&mut plan)? {
            println!("Stopped before splitting. The plan is saved at {}; `split` it when ready.", plan_path.display());
            return Ok(());
        }
        plan.save(&plan_path)?;
    }
    write_output(&plan, &out)
}
```

- [ ] **Step 4: Wire into `src/cli/mod.rs`**

Add `mod run;`, then:

```rust
    /// Analyze, review and split in one go (prompts for anything left out)
    Run(run::Args),
```

```rust
        Cmd::Run(a) => run::run(a),
```

- [ ] **Step 5: Run the tests**

Run: `cargo test 2>&1 | tail -15`
Expected: all pass.

- [ ] **Step 6: Update the README and Makefile (minimum to stay correct)**

`Makefile`: change the `cli` target comment and keep it working:

```make
cli: $(MODEL)    ## Guided terminal session (same as `run`)
	cargo run -- run
```

`README.md`: change the intro sentence "A Rust application (local web GUI plus an interactive CLI)" to "A Rust application (local web GUI plus a full command-line interface that works over SSH)", the `make cli` row in the Running table to "Guided terminal session", and replace the whole `## Usage (CLI)` section with:

````markdown
## Usage (CLI)

The CLI does everything the GUI does, step by step, joined by a plan file. It works over SSH: nothing needs a display. Run `audiobook-splitter-rust --help` (or `cargo run -- --help`) for the commands, and `<command> --help` for each one's flags.

| Command | What it does |
|---|---|
| `analyze <input>` | Detect silences (and match EPUB chapters with `--epub`), write `plan.json` |
| `rescan <plan>` | Look again for chapters the EPUB match missed (`--chapter N` for one) |
| `edit <plan>` | Interactive menu: rename, remove, add or move chapters, edit tags and file naming |
| `preview <plan> <n>` | Write 30 s from break *n* to an MP3, to check by ear (`scp` it, or `--play` locally) |
| `split <plan> -o <dir>` | Write the tagged files (`--album`, `--artist`, `--template`, ...) |
| `run <input> -o <dir>` | Analyze, edit and split in one go (`--yes` skips the editor) |
| `gui` | Start the web GUI |

A typical remote session:

```bash
audiobook-splitter-rust analyze ~/books/mybook/ --epub mybook.epub --min-silence 1.5 -o mybook.plan.json
audiobook-splitter-rust edit mybook.plan.json
audiobook-splitter-rust split mybook.plan.json -o ~/books/mybook-split
```

Transcription is the slow step; the plan file means you do it once and can edit and split later (run it in `tmux` so a dropped connection doesn't lose it). A plan records the audio files it was made from and is rejected if they change. `run` with no arguments (or the old `--cli` flag) asks for the input, output and an optional EPUB. Running the binary with no arguments prints the help; the GUI needs `gui`.
````

Also fix the older README line "Run it with `--cli` for the terminal version." in the EPUB section to "The CLI does the same with `analyze --epub`." and the table row `make dev | cargo run | Debug build, starts the GUI` to `cargo run -- gui`, and `make run | cargo run --release` to `cargo run --release -- gui`, and update the Makefile's `dev` and `run` targets accordingly (`cargo run -- gui`, `cargo run --release -- gui`).

- [ ] **Step 7: Parity pass by hand against the GUI**

Using one real book (EPUB + audio, ideally two audio files), do each once in the CLI and compare with the GUI result:

1. `analyze --epub ... --min-silence 1.5`: same chapter count and titles as the GUI's analysis.
2. `rescan` on a missing chapter: found or "no match", same as the GUI's re-scan.
3. `edit`: rename, remove, add at a time (`1:02:03`), add at a skipped silence, move to previous/next silence, view heard text, edit tags.
4. `preview 3`: the clip plays the start of that chapter.
5. `split`: output names follow the template, tags are set (`ffprobe -show_entries format_tags`), a chapter spanning two files is one track, tracks are numbered across the whole book.
6. Kill `analyze` with Ctrl-C mid-way: no `plan.json` (and no `plan.json.tmp`) is left.
7. `ssh localhost` into a session, run `run` interactively, confirm the bars and menus render.

Fix anything that differs before the final commit.

- [ ] **Step 8: Commit**

```bash
git add src/cli README.md Makefile tests/cli.rs
git commit -m "Add run, keep --cli as an alias, update the README for the new CLI"
```

---

## Spec coverage check

| Spec requirement | Task |
|---|---|
| Shared `pipeline` module, GUI on top of it | 2, 3 |
| `plan` file: versioned, absolute paths, changed-file check, newer-version error, atomic write | 1 |
| `analyze` with `--epub/--min-silence/--noise-db/--threshold`, progress bars, summary table | 5 |
| `rescan` (all or `--chapter`) | 7 |
| `edit` (rename, remove, add at time, move, promote silence, tags/template, heard text, non-terminal hint) | 6 |
| `preview` clip file, optional `ffplay` | 7 |
| `split` with tag flags and progress | 5 |
| `run` chaining the stages, `--yes`, plan next to output | 8 |
| `gui` command; no args prints help; `--cli` alias | 4, 5, 8 |
| Fail-fast errors, non-zero exit | 4, 5 (and each command) |
| Tests first; GUI regression; manual parity checklist | every task; 3 (smoke), 8 (parity pass) |
| New dependencies only (clap, indicatif, dialoguer, assert_cmd) | 1 |
| README minimum edits | 8 |
