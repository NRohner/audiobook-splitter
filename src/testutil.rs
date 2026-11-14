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
