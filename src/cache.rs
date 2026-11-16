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

// Bump when a change to Whisper's parameters, the clip decoding in transcribe.rs, or the
// whisper-rs version could change transcripts; cached transcripts then stop matching, silences are kept.
pub const TRANSCRIBE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Entry {
    version: u32,
    // "<min_silence>|<noise_db>" -> the file's silences
    silences: BTreeMap<String, SilenceSet>,
    // "<model name>|<model bytes>|clip<seconds>|t<TRANSCRIBE_VERSION>" -> "<start_ms>|<end_ms>" -> (before, after)
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
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    let abs = std::fs::canonicalize(audio).or_else(|_| std::path::absolute(audio)).ok()?;
    let id = format!("{}|{}|{}", abs.display(), meta.len(), modified);
    Some(format!("{:016x}", fnv1a(id.as_bytes())))
}

// Exact round-trip formatting, so settings that differ at all get different keys.
fn settings_key(min_silence: f64, noise_db: f64) -> String {
    format!("{}|{}", min_silence, noise_db)
}

fn model_key(model: &Path) -> String {
    let name = model.file_name().unwrap_or_default().to_string_lossy();
    format!("{}|{}|clip{}|t{}", name, std::fs::metadata(model).map_or(0, |m| m.len()), CLIP_SECONDS, TRANSCRIBE_VERSION)
}

fn span_key((start, end): (f64, f64)) -> String {
    format!("{}|{}", (start * 1000.0).round() as i64, (end * 1000.0).round() as i64)
}

fn entry_path(dir: &Path, fingerprint: &str) -> PathBuf {
    dir.join(format!("{}.json", fingerprint))
}

// Only files named the way `entry_path` names them: 16 lowercase hex digits, then `.json`.
fn is_entry(path: &Path) -> bool {
    let is_fingerprint = |s: &str| s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    path.extension().is_some_and(|e| e == "json") && path.file_stem().and_then(|s| s.to_str()).is_some_and(is_fingerprint)
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
        // The process id keeps two processes writing the same entry off each other's temp file.
        let tmp = dir.join(format!("{}.{}.json.tmp", fp, std::process::id()));
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            std::fs::write(&tmp, serde_json::to_vec(&entry).map_err(std::io::Error::other)?)?;
            std::fs::rename(&tmp, entry_path(dir, &fp))
        };
        match write() {
            Ok(()) => None,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                (!self.warned.swap(true, Ordering::Relaxed))
                .then(|| format!("Cache not saved ({}): {}. Continuing without it.", dir.display(), e))
            }
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
        let mut targets: Vec<PathBuf> = if audio.is_empty() {
            std::fs::read_dir(dir)
                .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| is_entry(p)).collect())
                .unwrap_or_default()
        } else {
            audio.iter().filter_map(|a| fingerprint(a)).map(|fp| entry_path(dir, &fp)).filter(|p| p.is_file()).collect()
        };
        targets.sort();
        targets.dedup();
        let mut removed = 0;
        for p in &targets {
            match std::fs::remove_file(p) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("Failed to remove '{}': {}", p.display(), e)),
            }
        }
        Ok(removed)
    }
}

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
    fn a_setting_that_differs_past_three_decimals_misses() {
        let dir = temp_dir("cache-key");
        let wav = audio(&dir, "a.wav", 10);
        let cache = cache_in(&dir);
        cache.store_silences(&wav, 2.0, -40.0, &[silence(4.0, 6.0)], 15.0);
        assert!(cache.silences(&wav, 2.0, -40.0).is_some());
        assert!(cache.silences(&wav, 2.0004, -40.0).is_none(), "min silence is keyed exactly");
    }

    #[test]
    fn info_and_clear_ignore_files_that_are_not_entries() {
        let dir = temp_dir("cache-foreign");
        let (a, wav) = (audio(&dir, "a.wav", 10), audio(&dir, "b.wav", 10));
        let cache = cache_in(&dir);
        cache.store_silences(&a, 2.0, -40.0, &[], 1.0);
        let cache_dir = dir.join("cache");
        std::fs::write(cache_dir.join("notes.json"), "mine").unwrap();
        std::fs::write(cache_dir.join("0123456789abcdef.txt"), "mine").unwrap();
        std::fs::write(cache_dir.join("0123456789abcdef.json.tmp"), "half written").unwrap();

        assert_eq!(cache.info().entries, 1, "only the real entry counts");
        assert_eq!(cache.clear(&[]), Ok(1), "only the real entry is removed");
        assert!(cache_dir.join("notes.json").is_file());
        assert!(cache_dir.join("0123456789abcdef.txt").is_file());
        assert!(cache_dir.join("0123456789abcdef.json.tmp").is_file());
        assert!(cache.silences(&a, 2.0, -40.0).is_none(), "the stored entry was cleared");
        assert!(wav.is_file());
    }

    #[test]
    fn clear_with_the_same_entry_twice_removes_it_once() {
        let dir = temp_dir("cache-dup");
        let a = audio(&dir, "a.wav", 10);
        let cache = cache_in(&dir);
        cache.store_silences(&a, 2.0, -40.0, &[], 1.0);
        assert_eq!(cache.clear(&[a.clone(), a.clone()]), Ok(1));
        assert!(cache.silences(&a, 2.0, -40.0).is_none());
    }

    #[test]
    fn different_spellings_of_a_path_share_an_entry() {
        let dir = temp_dir("cache-canon");
        std::fs::create_dir(dir.join("sub")).unwrap();
        let a = audio(&dir, "a.wav", 10);
        let cache = cache_in(&dir);
        cache.store_silences(&dir.join("sub/../a.wav"), 2.0, -40.0, &[], 1.0);
        assert!(cache.silences(&a, 2.0, -40.0).is_some());
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
