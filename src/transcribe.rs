// Local speech-to-text with Whisper (whisper.cpp: Metal GPU on macOS, Vulkan GPU on Windows and Linux).
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

pub const MODEL_FILE: &str = "ggml-base.en.bin";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";

// `$WHISPER_MODEL`, else models/<MODEL_FILE> in the working directory or the repo
// the binary was built in (target/{debug,release}/../../models).
pub fn find_model() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("WHISPER_MODEL") {
        return Ok(PathBuf::from(path));
    }
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let candidates = [
        Some(PathBuf::from("models")),
        exe_dir.clone().map(|d| d.join("models")),
        exe_dir.map(|d| d.join("../../models")),
    ];
    candidates
        .into_iter()
        .flatten()
        .map(|dir| dir.join(MODEL_FILE))
        .find(|p| p.is_file())
        .ok_or_else(|| format!("Whisper model '{}' not found. Download it into models/ (`make model`, or see the README on Windows).", MODEL_FILE))
}

// Like `find_model`, but downloads the default model with curl (on every supported OS)
// when it isn't there yet. A `$WHISPER_MODEL` path is the user's choice and never fetched.
pub fn ensure_model() -> Result<PathBuf, String> {
    let missing = match find_model() {
        Ok(path) => return Ok(path),
        Err(e) if std::env::var_os("WHISPER_MODEL").is_some() => return Err(e),
        Err(e) => e,
    };
    let dir = download_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{} Could not create '{}': {}", missing, dir.display(), e))?;
    let target = dir.join(MODEL_FILE);
    let partial = dir.join(format!("{}.part", MODEL_FILE));
    // Silent so it can't scribble over the terminal UI; -f turns HTTP errors into failures.
    let output = Command::new("curl")
        .args(["-L", "-f", "-sS", "-o"])
        .arg(&partial)
        .arg(MODEL_URL)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{} Automatic download needs curl, which failed to run: {}", missing, e))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("{} Downloading it failed: {}", missing, String::from_utf8_lossy(&output.stderr).trim()));
    }
    std::fs::rename(&partial, &target).map_err(|e| format!("Could not save the model to '{}': {}", target.display(), e))?;
    Ok(target)
}

// models/ in the repo the binary was built in (target/{debug,release}/../../models), so
// the download lands where `make model` puts it; otherwise models/ in the working directory.
fn download_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("../..")))
        .filter(|root| root.join("Cargo.toml").is_file())
        .map(|root| root.join("models"))
        .unwrap_or_else(|| PathBuf::from("models"))
}

// On an M3 Pro (Metal), 3 transcribed a book 8% faster than 2 (39 s vs 42 s; one at a time took 78 s).
const GPU_STATES: usize = 3;

// Whether ggml found a GPU at runtime. A GPU build with no usable device (e.g. Vulkan
// without a driver) lists only the CPU, so it's treated like a CPU-only build.
fn gpu_found() -> bool {
    use whisper_rs_sys::*;
    // SAFETY: read-only queries of ggml's static device registry, indices in range.
    (0..unsafe { ggml_backend_dev_count() }).any(|i| {
        let kind = unsafe { ggml_backend_dev_type(ggml_backend_dev_get(i)) };
        kind == ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU
            || kind == ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU
    })
}

// Up to `states` transcriptions share one loaded model, so a GPU can run them side by
// side. A CPU build runs one: Whisper already uses several cores per transcription.
// Each clip gets a fresh state: a reused one carries over enough from earlier clips to
// change a word now and then, which would make results depend on clip order.
pub struct Transcriber {
    ctx: WhisperContext,
    running: Mutex<usize>,
    finished: Condvar,
    states: usize,
}

impl Transcriber {
    pub fn load(model: &Path) -> Result<Self, String> {
        whisper_rs::install_logging_hooks(); // Keep whisper.cpp's logs out of the terminal.
        let model_str = model.to_str().ok_or("Model path is not valid UTF-8")?;
        let ctx = WhisperContext::new_with_params(model_str, WhisperContextParameters::default())
            .map_err(|e| format!("Failed to load Whisper model '{}': {}", model.display(), e))?;
        let states = if gpu_found() { GPU_STATES } else { 1 };
        Ok(Self { ctx, running: Mutex::new(0), finished: Condvar::new(), states })
    }

    // How many transcriptions can run at once.
    pub fn states(&self) -> usize {
        self.states
    }

    // Transcribes 16 kHz mono samples. Safe to call from several threads; waits while
    // `states` transcriptions are already running.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, String> {
        if samples.is_empty() {
            return Ok(String::new());
        }
        *self.finished.wait_while(self.running.lock().unwrap(), |n| *n == self.states).unwrap() += 1;
        let text = self
            .ctx
            .create_state()
            .map_err(|e| format!("Failed to init Whisper: {}", e))
            .and_then(|mut state| Self::run(&mut state, samples));
        *self.running.lock().unwrap() -= 1;
        self.finished.notify_one();
        text
    }

    fn run(state: &mut WhisperState, samples: &[f32]) -> Result<String, String> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_no_context(true); // Each clip is independent.
        params.set_suppress_blank(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Changing these parameters can change transcripts: bump cache::TRANSCRIBE_VERSION.
        state.full(params, samples).map_err(|e| format!("Whisper failed: {}", e))?;
        Ok(state.as_iter().map(|seg| seg.to_string()).collect::<Vec<_>>().join(" "))
    }
}

// The decode/resample chain here affects transcripts: bump cache::TRANSCRIBE_VERSION when it changes.
// Decodes `duration` seconds of `input` starting at `start` into 16 kHz mono f32 samples.
pub fn clip_samples(input: &Path, start: f64, duration: f64) -> Result<Vec<f32>, String> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-ss", &start.max(0.0).to_string(), "-t", &duration.to_string(), "-i"])
        .arg(input)
        .args(["-ac", "1", "-ar", "16000", "-f", "f32le", "-"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;
    if !output.status.success() {
        return Err(format!("FFmpeg failed to decode clip:\n{}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(output.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matching::{bigram_score, normalize};

    // Writes `text` as speech to a temp file with the OS text-to-speech engine.
    #[cfg(target_os = "macos")]
    fn speak(text: &str) -> PathBuf {
        let aiff = std::env::temp_dir().join(format!("abs-say-{}-{}.aiff", std::process::id(), text.len()));
        assert!(Command::new("say").arg("-o").arg(&aiff).arg(text).status().unwrap().success());
        aiff
    }

    #[cfg(windows)]
    fn speak(text: &str) -> PathBuf {
        let wav = std::env::temp_dir().join(format!("abs-say-{}-{}.wav", std::process::id(), text.len()));
        let script = format!(
            "Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
             $s.SetOutputToWaveFile('{}'); $s.Speak('{}'); $s.Dispose()",
            wav.display(),
            text.replace('\'', "''")
        );
        assert!(Command::new("powershell").args(["-NoProfile", "-Command", &script]).status().unwrap().success());
        wav
    }

    // Needs the model (`make model`) and the OS text-to-speech engine: cargo test -- --ignored
    #[test]
    #[ignore]
    fn transcribes_speech_close_to_text() {
        let text = "It was a bright cold day in April, and the clocks were striking thirteen. \
                    Winston Smith slipped quickly through the glass doors of Victory Mansions.";
        let audio = speak(text);

        let samples = clip_samples(&audio, 0.0, 30.0).unwrap();
        assert!(samples.len() > 16000);
        let t = Transcriber::load(&find_model().unwrap()).unwrap();
        let started = std::time::Instant::now();
        let transcript = t.transcribe(&samples).unwrap();
        println!("{:?} in {:?}", transcript, started.elapsed());
        let score = bigram_score(&normalize(&transcript), &normalize(text));
        assert!(score > 0.7, "score {} for {:?}", score, transcript);
    }

    // Parallel transcription must give the same text, in candidate order, as one clip
    // at a time. Needs the model and text-to-speech: cargo test -- --ignored
    #[test]
    #[ignore]
    fn parallel_transcripts_match_sequential() {
        use crate::chapters::{transcribe_candidates, AudioFile, Candidate};
        let audio = speak(
            "Call me Ishmael. Some years ago, never mind how long precisely, having little or no money in my purse, \
             and nothing particular to interest me on shore, I thought I would sail about a little and see the \
             watery part of the world. It is a way I have of driving off the spleen and regulating the circulation.",
        );
        let duration = clip_samples(&audio, 0.0, 600.0).unwrap().len() as f64 / 16000.0;
        let files = [AudioFile { path: audio.clone(), silences: vec![], duration }];
        let cands: Vec<Candidate> = [0.0, 0.2, 0.35, 0.5, 0.65, 0.8]
            .iter()
            .map(|f| Candidate { file: 0, start: duration * f, end: duration * f + 0.3 })
            .collect();

        let t = Transcriber::load(&find_model().unwrap()).unwrap();
        let clip = |from: f64, to: f64| {
            if to - from < 0.5 { String::new() } else { t.transcribe(&clip_samples(&audio, from, to - from).unwrap()).unwrap() }
        };
        let expected: Vec<(String, String)> =
            cands.iter().map(|c| (clip(0.0, c.start), clip(c.end, duration))).collect();

        let started = std::time::Instant::now();
        let mut calls = 0;
        let got = transcribe_candidates(&files, &cands, &t, |_, _| calls += 1).unwrap();
        println!("{} candidates with {} states in {:?}", cands.len(), t.states(), started.elapsed());
        assert_eq!(got, expected);
        assert_eq!(calls, cands.len());
    }
}
