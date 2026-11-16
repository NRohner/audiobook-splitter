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
