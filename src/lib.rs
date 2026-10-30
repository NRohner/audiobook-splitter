// Core audio-splitting logic shared by the CLI and the GUI.
pub mod book;
pub mod chapters;
pub mod matching;
pub mod meta;
pub mod transcribe;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const AUDIO_EXTENSIONS: [&str; 7] = ["mp3", "m4b", "wav", "flac", "aac", "m4a", "ogg"];

// A path the user typed or pasted, without surrounding whitespace or the quotes
// Windows Explorer's "Copy as path" adds.
pub fn user_path(input: &str) -> PathBuf {
    let s = input.trim();
    let unquoted = ['"', '\''].iter().find_map(|&q| s.strip_prefix(q)?.strip_suffix(q));
    PathBuf::from(unquoted.unwrap_or(s))
}

// A detected silence region, in seconds.
#[derive(Debug, PartialEq)]
pub struct Silence {
    pub start: f64,
    pub end: f64,
    pub duration: f64,
}

// Returns the supported audio files directly inside `dir`, sorted by file name.
pub fn scan_folder(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("Failed to read folder '{}': {}", dir.display(), e))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        })
        .collect();
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    Ok(files)
}

// A file path yields just that file; a folder yields its audio files.
pub fn collect_inputs(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        Ok(vec![path.to_path_buf()])
    } else if path.is_dir() {
        scan_folder(path)
    } else {
        Err(format!("'{}' is not a file or folder.", path.display()))
    }
}

// Parses FFmpeg `silencedetect` log output into silence regions.
pub fn parse_silencedetect(log: &str) -> Vec<Silence> {
    let re_start = Regex::new(r"silence_start: (?P<start>-?\d+(\.\d+)?)").unwrap();
    let re_end = Regex::new(
        r"silence_end: (?P<end>-?\d+(\.\d+)?) \| silence_duration: (?P<duration>-?\d+(\.\d+)?)",
    )
    .unwrap();

    let mut starts: Vec<f64> = Vec::new();
    let mut silences = Vec::new();
    for line in log.lines() {
        if let Some(cap) = re_start.captures(line) {
            starts.push(cap["start"].parse().unwrap());
        } else if let Some(cap) = re_end.captures(line) {
            if let Some(start) = starts.pop() {
                silences.push(Silence {
                    start,
                    end: cap["end"].parse().unwrap(),
                    duration: cap["duration"].parse().unwrap(),
                });
            }
        }
    }
    silences
}

// Split at the midpoint of each long-enough silence, plus the end of the file.
// Empty when there is nothing to split.
pub fn split_points(silences: &[Silence], min_silence: f64, total_duration: f64) -> Vec<f64> {
    let mut points: Vec<f64> = silences
        .iter()
        .filter(|s| s.duration >= min_silence)
        .map(|s| s.start + s.duration / 2.0)
        .collect();
    if points.last().is_some_and(|&last| last < total_duration - 0.01) {
        points.push(total_duration);
    }
    points
}

// Next free index for `<stem>_NNN.<ext>` files in `dir`, so nothing gets overwritten.
pub fn next_file_index(dir: &Path, stem: &str, ext: &str) -> Result<usize, String> {
    let pattern = format!(r"^{}_(?P<index>\d{{3,}})\.{}$", regex::escape(stem), regex::escape(ext));
    let file_regex = Regex::new(&pattern).map_err(|e| format!("Failed to build index regex: {}", e))?;

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(1); // Folder doesn't exist yet.
    };
    let max_index = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            file_regex.captures(&name)?["index"].parse::<usize>().ok()
        })
        .max()
        .unwrap_or(0);
    Ok(max_index + 1)
}

// Runs FFmpeg silence detection and FFprobe, returning the silences and total duration.
pub fn detect_silences(
    input: &Path,
    min_silence: f64,
    noise_db: f64,
) -> Result<(Vec<Silence>, f64), String> {
    let silences = silencedetect(input, &[], min_silence, noise_db)?;
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(input)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ffprobe. Is FFprobe installed and on your PATH? ({})", e))?;
    if !probe.status.success() {
        return Err(format!("FFprobe failed:\n{}", String::from_utf8_lossy(&probe.stderr)));
    }
    let total_duration = String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse::<f64>()
        .map_err(|e| format!("Failed to parse audio duration: {}", e))?;

    Ok((silences, total_duration))
}

// Silences between `from` and `to` seconds of `input`, in times from the file's start.
pub fn detect_silences_between(input: &Path, from: f64, to: f64, min_silence: f64, noise_db: f64) -> Result<Vec<Silence>, String> {
    let range = ["-ss".to_string(), from.to_string(), "-t".to_string(), (to - from).to_string()];
    let mut silences = silencedetect(input, &range, min_silence, noise_db)?;
    for s in &mut silences {
        s.start += from;
        s.end += from;
    }
    Ok(silences)
}

// Runs FFmpeg `silencedetect`; `input_args` go before `-i` (e.g. a seek).
fn silencedetect(input: &Path, input_args: &[String], min_silence: f64, noise_db: f64) -> Result<Vec<Silence>, String> {
    let output = Command::new("ffmpeg")
        .arg("-hide_banner")
        .args(input_args)
        .arg("-i")
        .arg(input)
        .arg("-af")
        .arg(format!("silencedetect=n={}dB:d={}", noise_db, min_silence))
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ffmpeg. Is FFmpeg installed and on your PATH? ({})", e))?;
    let log = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!("FFmpeg silence detection failed:\n{}", log));
    }
    Ok(parse_silencedetect(&log))
}

// Decodes `seconds` of `input` from `start` into MP3 bytes, playable in any browser.
pub fn preview_clip(input: &Path, start: f64, seconds: f64) -> Result<Vec<u8>, String> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-ss", &start.to_string(), "-i"])
        .arg(input)
        .args(["-t", &seconds.to_string(), "-vn", "-f", "mp3", "pipe:1"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;
    if !output.status.success() {
        return Err(format!("FFmpeg failed to cut a preview:\n{}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(output.stdout)
}

// Writes one `<stem>_NNN.<ext>` file per segment into `out_dir`.
// With `tracks`, segment i is named and tagged by `tracks[i]` (never overwriting an existing file).
// `on_part(part, total, path)` is called before each part is written. Returns the number of files written.
pub fn split_file(
    input: &Path,
    out_dir: &Path,
    points: &[f64],
    tracks: Option<&[meta::Track]>,
    mut on_part: impl FnMut(usize, usize, &Path),
) -> Result<usize, String> {
    let ext = input.extension().unwrap_or_default().to_string_lossy().to_string();
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("audio_part");
    let mut index = next_file_index(out_dir, stem, &ext)?;
    let no_video = ["m4b", "m4a"].contains(&ext.to_lowercase().as_str());
    let mut start = 0.0;
    let mut written = 0;

    for (i, &end) in points.iter().enumerate() {
        let duration = end - start;
        if duration <= 0.01 {
            start = end;
            continue;
        }
        let track = tracks.and_then(|t| t.get(i));
        let out_path = match track {
            Some(track) => unique_path(out_dir, &track.name, &ext),
            None => out_dir.join(format!("{}_{:03}.{}", stem, index, ext)),
        };
        on_part(i + 1, points.len(), &out_path);

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(input)
            .args(["-ss", &start.to_string(), "-t", &duration.to_string(), "-c", "copy"]);
        // ponytail: drops cover art from M4B/M4A parts; the ipod muxer rejects cover
        // tracks stored as MJPEG video. Re-attach covers with a remux if anyone misses them.
        if no_video {
            cmd.arg("-vn");
        }
        for (key, value) in track.map_or(&[][..], |t| &t.tags) {
            cmd.arg("-metadata").arg(format!("{}={}", key, value));
        }
        let output = cmd
            .arg("-y")
            .arg(&out_path)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("Failed to run ffmpeg for splitting: {}", e))?;
        if !output.status.success() {
            let _ = std::fs::remove_file(&out_path); // Don't leave a broken part behind.
            return Err(format!(
                "FFmpeg failed on part {}:\n{}",
                i + 1,
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        start = end;
        index += 1;
        written += 1;
    }
    Ok(written)
}

// `<dir>/<name>.<ext>`, or `<name> (2).<ext>` etc. if that already exists.
pub fn unique_path(dir: &Path, name: &str, ext: &str) -> PathBuf {
    let mut path = dir.join(format!("{}.{}", name, ext));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{} ({}).{}", name, n, ext));
        n += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("abs-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn s(start: f64, duration: f64) -> Silence {
        Silence { start, end: start + duration, duration }
    }

    #[test]
    fn user_path_strips_surrounding_quotes() {
        assert_eq!(user_path(r#" "C:\Books\Dune.m4b" "#), PathBuf::from(r"C:\Books\Dune.m4b"));
        assert_eq!(user_path("'/tmp/a b.mp3'"), PathBuf::from("/tmp/a b.mp3"));
        assert_eq!(user_path("  plain.mp3\n"), PathBuf::from("plain.mp3"));
        assert_eq!(user_path(r#""half.mp3"#), PathBuf::from(r#""half.mp3"#));
        assert_eq!(user_path(""), PathBuf::from(""));
    }

    #[test]
    fn parses_silencedetect_output() {
        let log = "\
[silencedetect @ 0x1] silence_start: 0
[silencedetect @ 0x1] silence_end: 1.5 | silence_duration: 1.5
size=N/A time=00:00:10.00
[silencedetect @ 0x1] silence_start: -0.00133
[silencedetect @ 0x1] silence_end: 2.25 | silence_duration: 2.25133
[silencedetect @ 0x1] silence_start: 12.75
[silencedetect @ 0x1] silence_end: 15.25 | silence_duration: 2.5";
        assert_eq!(
            parse_silencedetect(log),
            vec![
                Silence { start: 0.0, end: 1.5, duration: 1.5 },
                Silence { start: -0.00133, end: 2.25, duration: 2.25133 },
                Silence { start: 12.75, end: 15.25, duration: 2.5 },
            ]
        );
    }

    #[test]
    fn split_points_use_silence_midpoints_and_end() {
        let points = split_points(&[s(10.0, 2.0), s(30.0, 4.0)], 2.0, 60.0);
        assert_eq!(points, vec![11.0, 32.0, 60.0]);
    }

    #[test]
    fn split_points_ignore_short_silences() {
        assert_eq!(split_points(&[s(10.0, 1.0), s(30.0, 4.0)], 2.0, 60.0), vec![32.0, 60.0]);
    }

    #[test]
    fn split_points_empty_without_silences() {
        assert!(split_points(&[], 2.0, 60.0).is_empty());
        assert!(split_points(&[s(10.0, 1.0)], 2.0, 60.0).is_empty());
    }

    #[test]
    fn split_points_no_duplicate_end() {
        // Silence midpoint lands on the end of the file.
        assert_eq!(split_points(&[s(58.0, 4.0)], 2.0, 60.0), vec![60.0]);
    }

    #[test]
    fn detect_silences_between_reports_file_times() {
        let dir = temp_dir("between");
        let wav = dir.join("gap.wav");
        let made = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "aevalsrc=if(between(t\\,4\\,6)\\,0\\,sin(2*PI*440*t)):d=10", "-y"])
            .arg(&wav)
            .status()
            .unwrap();
        assert!(made.success());
        let found = detect_silences_between(&wav, 2.0, 9.0, 1.0, -40.0).unwrap();
        assert_eq!(found.len(), 1, "{:?}", found);
        assert!((found[0].start - 4.0).abs() < 0.1 && (found[0].end - 6.0).abs() < 0.1, "{:?}", found);
    }

    #[test]
    fn preview_clip_returns_mp3() {
        let dir = temp_dir("clip");
        let wav = dir.join("tone.wav");
        let made = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "sine=d=5", "-y"])
            .arg(&wav)
            .status()
            .unwrap();
        assert!(made.success());
        let mp3 = preview_clip(&wav, 1.0, 2.0).unwrap();
        assert!(mp3.starts_with(b"ID3"), "not an MP3");
        assert!(preview_clip(&dir.join("missing.wav"), 0.0, 2.0).is_err());
    }

    #[test]
    fn split_file_handles_m4b_with_cover_art() {
        // Cover as an MJPEG video track (Audible-style "timed thumbnail"), which the
        // ipod muxer behind .m4b/.m4a can't store.
        let dir = temp_dir("m4b");
        let ffmpeg = |args: &[&str]| {
            let ok = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).current_dir(&dir).status();
            assert!(ok.unwrap().success());
        };
        ffmpeg(&["-f", "lavfi", "-i", "color=c=red:s=64x64", "-frames:v", "1", "cover.jpg"]);
        ffmpeg(&["-f", "lavfi", "-i", "sine=d=6", "-i", "cover.jpg", "-map", "0", "-map", "1", "-c:a", "aac",
            "-c:v", "copy", "-f", "mp4", "book.m4b"]);
        let out = dir.join("out");
        fs::create_dir(&out).unwrap();
        assert_eq!(split_file(&dir.join("book.m4b"), &out, &[3.0, 6.0], None, |_, _, _| {}), Ok(2));
    }

    #[test]
    fn split_file_writes_track_names_and_tags() {
        let dir = temp_dir("tags");
        let mp3 = dir.join("book.mp3");
        let made = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "sine=d=4", "-metadata", "genre=Source", "-y"])
            .arg(&mp3)
            .status()
            .unwrap();
        assert!(made.success());
        let track = |name: &str, n: usize| meta::Track {
            name: name.into(),
            tags: vec![("title".into(), name.into()), ("track".into(), format!("{}/2", n)), ("disc".into(), "1/1".into())],
        };
        let out = dir.join("out");
        fs::create_dir(&out).unwrap();
        let tracks = [track("00 - Opening", 1), track("01 - One", 2)];
        assert_eq!(split_file(&mp3, &out, &[2.0, 4.0], Some(&tracks), |_, _, _| {}), Ok(2));

        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "format_tags=title,track,disc,genre", "-of", "default=noprint_wrappers=1"])
            .arg(out.join("01 - One.mp3"))
            .output()
            .unwrap();
        let tags = String::from_utf8_lossy(&probe.stdout).to_lowercase();
        for want in ["title=01 - one", "track=2/2", "disc=1/1", "genre=source"] {
            assert!(tags.contains(want), "missing {} in {}", want, tags);
        }
    }

    #[test]
    fn scan_folder_filters_and_sorts() {
        let dir = temp_dir("scan");
        for name in ["b.MP3", "a.wav", "notes.txt", "c.flac", "d.m4b"] {
            fs::write(dir.join(name), b"").unwrap();
        }
        fs::create_dir(dir.join("sub.mp3")).unwrap();
        let names: Vec<_> = scan_folder(&dir)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.wav", "b.MP3", "c.flac", "d.m4b"]);
    }

    #[test]
    fn collect_inputs_handles_file_folder_and_missing() {
        let dir = temp_dir("collect");
        let file = dir.join("x.mp3");
        fs::write(&file, b"").unwrap();
        assert_eq!(collect_inputs(&file).unwrap(), vec![file.clone()]);
        assert_eq!(collect_inputs(&dir).unwrap(), vec![file]);
        assert!(collect_inputs(&dir.join("nope")).is_err());
    }

    #[test]
    fn unique_path_never_reuses_existing_names() {
        let dir = temp_dir("unique");
        assert_eq!(unique_path(&dir, "01 - One", "mp3"), dir.join("01 - One.mp3"));
        fs::write(dir.join("01 - One.mp3"), b"").unwrap();
        fs::write(dir.join("01 - One (2).mp3"), b"").unwrap();
        assert_eq!(unique_path(&dir, "01 - One", "mp3"), dir.join("01 - One (3).mp3"));
    }

    #[test]
    fn next_file_index_continues_after_highest() {
        let dir = temp_dir("index");
        assert_eq!(next_file_index(&dir, "book (1)", "mp3").unwrap(), 1);
        for name in ["book (1)_001.mp3", "book (1)_007.mp3", "book (1)_009.wav", "other_020.mp3"] {
            fs::write(dir.join(name), b"").unwrap();
        }
        assert_eq!(next_file_index(&dir, "book (1)", "mp3").unwrap(), 8);
        assert_eq!(next_file_index(&dir.join("missing"), "book", "mp3").unwrap(), 1);
    }
}
