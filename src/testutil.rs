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

// A 64x64 picture of one colour ("red", "green", "blue"); the extension picks JPEG or PNG.
pub fn image(path: &Path, color: &str) {
    let made = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &format!("color=c={}:s=64x64", color), "-frames:v", "1", "-y"])
        .arg(path)
        .status()
        .unwrap();
    assert!(made.success());
}

// A sine-tone MP3 or M4B (by extension), with `cover` embedded if given.
pub fn audio_with_cover(path: &Path, seconds: u32, cover: Option<&Path>) {
    let codec = if path.extension().is_some_and(|e| e == "mp3") { "libmp3lame" } else { "aac" };
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &format!("sine=d={}", seconds)]);
    if let Some(cover) = cover {
        cmd.arg("-i").arg(cover).args(["-map", "0:a", "-map", "1:v", "-c:v", "copy", "-disposition:v:0", "attached_pic"]);
    }
    let made = cmd.args(["-c:a", codec, "-y"]).arg(path).status().unwrap();
    assert!(made.success());
}

// The dominant colour of the first picture in `path`, or None if it has no picture.
pub fn cover_color(path: &Path) -> Option<&'static str> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-frames:v", "1", "-vf", "scale=1:1", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
        .output()
        .ok()?;
    let pixel = out.stdout.get(..3)?;
    let strongest = pixel.iter().enumerate().max_by_key(|(_, v)| **v)?.0;
    Some(["red", "green", "blue"][strongest])
}

// A format tag such as "title", or "" if there is none.
pub fn tag(path: &Path, key: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", &format!("format_tags={}", key), "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
