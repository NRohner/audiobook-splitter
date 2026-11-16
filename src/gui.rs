// Local web GUI: a tiny HTTP server on 127.0.0.1 serving one embedded page.
// Native file dialogs run in a child process (`--pick`), so this process never becomes
// a Cocoa app whose unpumped event loop macOS would flag as not responding;
// analysis and splitting run on a background thread and the page polls /status.
use audiobook_splitter_rust::book::{book_info, load_chapters};
use audiobook_splitter_rust::chapters::{
    candidates, chapter_breaks, pick_breaks, plan_from_breaks, rescan_chapter, rescan_window, silence_breaks,
    transcribe_candidates, window_candidates, window_ranges, AudioFile, Break, Candidate, ChapterBreak,
};
use audiobook_splitter_rust::meta::{tracks, Metadata, DEFAULT_TEMPLATE};
use audiobook_splitter_rust::transcribe::{find_model, Transcriber};
use audiobook_splitter_rust::{collect_inputs, detect_silences, detect_silences_between, preview_clip, split_points, user_path, write_segment, AUDIO_EXTENSIONS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use tiny_http::{Header, Method, Request, Response, Server};

const PAGE: &str = include_str!("ui.html");
const DEFAULT_PORT: u16 = 7878;
const CLIP_SECONDS: f64 = 30.0;
// ponytail: fixed loosening for re-scans; make them settings if one step isn't enough.
const RESCAN_SILENCE_FACTOR: f64 = 0.5;
const RESCAN_NOISE_DB: f64 = 10.0;

#[derive(Serialize, Clone)]
struct FileRow {
    name: String,
    error: Option<String>,
}

// A chapter start suggested by the analysis; the page edits its own copy.
#[derive(Serialize)]
struct BreakRow {
    file: usize, // Index into `audio`.
    at: f64,
    title: String,
    score: Option<f64>, // EPUB match score.
}

// A silence the page can turn into a chapter break, with what was heard (EPUB only).
#[derive(Serialize)]
struct SilenceRow {
    file: usize,
    at: f64,
    before: String,
    after: String,
}

// An EPUB chapter with no break; `tried` once a re-scan also came up empty.
#[derive(Serialize)]
struct MissingRow {
    chapter: usize,
    title: String,
    tried: bool,
}

// What an EPUB analysis used, kept so missing chapters can be re-scanned.
struct EpubRun {
    chapters: Vec<audiobook_splitter_rust::book::Chapter>,
    model: std::path::PathBuf,
    min_silence: f64,
    noise_db: f64,
    threshold: f64,
}

#[derive(Serialize)]
struct AudioRow {
    name: String,
    duration: f64,
}

#[derive(Serialize, Default)]
struct Status {
    running: bool,
    phase: &'static str, // "analyze", "split" or "" before anything has run
    stage: String,       // What the progress bar is counting right now.
    done: usize,
    total: usize,
    run: usize,          // Counts analyses, so the page knows when to reload its editor.
    files: Vec<FileRow>, // Every input, with any error.
    audio: Vec<AudioRow>, // The analyzed files; `file` in breaks and silences indexes these.
    breaks: Vec<BreakRow>,
    silences: Vec<SilenceRow>,
    missing: Vec<MissingRow>, // EPUB chapters with no matching break.
    meta: Metadata,       // Suggested tags.
    log: Vec<String>,
    #[serde(skip)]
    analyzed: Vec<AudioFile>, // Same order as `audio`.
    #[serde(skip)]
    epub: Option<Arc<EpubRun>>,
    #[serde(skip)]
    found: Vec<ChapterBreak>, // Matched chapters, including re-scan finds.
}

type Shared = Arc<Mutex<Status>>;

#[derive(Deserialize)]
struct AnalyzeReq {
    input: String,
    min_silence: f64,
    noise_db: f64,
    #[serde(default)]
    epub: String,
    #[serde(default = "default_threshold")]
    threshold: f64,
}

fn default_threshold() -> f64 {
    0.4
}

#[derive(Deserialize)]
struct SplitReq {
    output: String,
    breaks: Vec<Break>,
    meta: Metadata,
}

#[derive(Deserialize)]
struct RescanReq {
    chapters: Vec<usize>,
}

#[derive(Deserialize)]
struct BrowseReq {
    kind: String, // "file", "epub" or "folder"
}

pub fn run() {
    // ponytail: fixed port so a reload keeps working; falls back to any free port.
    let server = Server::http(("127.0.0.1", DEFAULT_PORT))
        .or_else(|_| Server::http("127.0.0.1:0"))
        .expect("Failed to start the GUI server");
    let port = server.server_addr().to_ip().unwrap().port();
    let url = format!("http://127.0.0.1:{}", port);
    println!("Audio Splitter GUI running at {} (Ctrl+C to quit)", url);
    open_browser(&url);

    let state: Shared = Arc::default();
    for req in server.incoming_requests() {
        handle(req, &state, port);
    }
}

fn open_browser(url: &str) {
    let result = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn()
    } else if cfg!(windows) {
        Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else {
        Command::new("xdg-open").arg(url).spawn()
    };
    if result.is_err() {
        println!("Open {} in your browser.", url);
    }
}

fn handle(mut req: Request, state: &Shared, port: u16) {
    // Only answer requests addressed to us, so other websites can't drive the
    // app (DNS rebinding). POSTs must be JSON, which forces a CORS preflight
    // that we never approve, so cross-site form posts are rejected too.
    let host_ok = header(&req, "Host")
        .is_some_and(|h| h == format!("127.0.0.1:{}", port) || h == format!("localhost:{}", port));
    let json_ok = header(&req, "Content-Type").is_some_and(|c| c.starts_with("application/json"));
    if !host_ok || (*req.method() == Method::Post && !json_ok) {
        let _ = req.respond(Response::from_string("Forbidden").with_status_code(403));
        return;
    }

    let mut body = String::new();
    let _ = req.as_reader().read_to_string(&mut body);

    let url = req.url().to_string();
    let (route, query) = url.split_once('?').unwrap_or((&url, ""));
    let result = match (req.method(), route) {
        (Method::Get, "/") => {
            let html = Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap();
            let _ = req.respond(Response::from_string(PAGE).with_header(html));
            return;
        }
        (Method::Get, "/status") => Ok(serde_json::to_value(&*state.lock().unwrap()).unwrap()),
        (Method::Get, "/clip") => match clip(state, query) {
            Ok(mp3) => {
                respond_audio(req, mp3);
                return;
            }
            Err(e) => Err(e),
        },
        (Method::Post, "/browse") => parse(&body).and_then(browse),
        (Method::Post, "/analyze") => parse(&body).and_then(|r| start_analyze(r, state)),
        (Method::Post, "/rescan") => parse(&body).and_then(|r| start_rescan(r, state)),
        (Method::Post, "/split") => parse(&body).and_then(|r| start_split(r, state)),
        _ => {
            let _ = req.respond(Response::from_string("Not found").with_status_code(404));
            return;
        }
    };

    let (code, value) = match result {
        Ok(v) => (200, v),
        Err(e) => (400, json!({ "error": e })),
    };
    let ct = Header::from_bytes("Content-Type", "application/json").unwrap();
    let _ = req.respond(Response::from_string(value.to_string()).with_status_code(code).with_header(ct));
}

// Safari only plays <audio> from servers that honour Range requests.
// ponytail: each range request re-cuts the clip; cache the last clip if that gets slow.
fn respond_audio(req: Request, mp3: Vec<u8>) {
    let len = mp3.len();
    let range = byte_range(header(&req, "Range").as_deref(), len);
    let (code, body) = match range {
        Some((start, end)) => (206, mp3[start..=end].to_vec()),
        None => (200, mp3),
    };
    let mut response = Response::from_data(body)
        .with_status_code(code)
        .with_header(Header::from_bytes("Content-Type", "audio/mpeg").unwrap())
        .with_header(Header::from_bytes("Accept-Ranges", "bytes").unwrap());
    if let Some((start, end)) = range {
        let value = format!("bytes {}-{}/{}", start, end, len);
        response = response.with_header(Header::from_bytes("Content-Range", value).unwrap());
    }
    let _ = req.respond(response);
}

// `bytes=start-[end]` as an inclusive range within `len` bytes; None means send everything.
fn byte_range(header: Option<&str>, len: usize) -> Option<(usize, usize)> {
    let (start, end) = header?.strip_prefix("bytes=")?.split_once('-')?;
    let start: usize = start.parse().ok()?;
    let last = len.checked_sub(1)?;
    let end = if end.is_empty() { last } else { end.parse::<usize>().ok()?.min(last) };
    (start <= end).then_some((start, end))
}

fn header(req: &Request, name: &'static str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_string())
}

fn parse<T: for<'de> Deserialize<'de>>(body: &str) -> Result<T, String> {
    serde_json::from_str(body).map_err(|e| format!("Bad request: {}", e))
}

fn browse(req: BrowseReq) -> Result<Value, String> {
    let output = std::env::current_exe()
        .and_then(|exe| Command::new(exe).args(["--pick", &req.kind]).output())
        .map_err(|e| format!("Failed to open the file dialog: {}", e))?;
    let path = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(json!({ "path": Some(path).filter(|p| !p.is_empty()) }))
}

// Runs in the `--pick` child process: shows one dialog and prints the chosen path.
pub fn pick(kind: &str) {
    let dialog = rfd::FileDialog::new();
    let picked = match kind {
        "file" => dialog.add_filter("Audio", &AUDIO_EXTENSIONS).pick_file(),
        "epub" => dialog.add_filter("EPUB", &["epub"]).pick_file(),
        _ => dialog.pick_folder(),
    };
    if let Some(path) = picked {
        print!("{}", path.to_string_lossy());
    }
}

// `file=N&at=T`: CLIP_SECONDS of analyzed file N from T seconds. Takes an index
// rather than a path so the page can only play files from the analysis.
fn clip(state: &Shared, query: &str) -> Result<Vec<u8>, String> {
    let param = |key: &str| query.split('&').find_map(|p| p.strip_prefix(key)?.strip_prefix('='));
    let file = param("file").and_then(|n| n.parse::<usize>().ok());
    let at = param("at").and_then(|t| t.parse::<f64>().ok()).filter(|t| *t >= 0.0);
    let path = file
        .zip(at)
        .and_then(|(file, at)| state.lock().unwrap().analyzed.get(file).filter(|f| at < f.duration).map(|f| f.path.clone()))
        .ok_or("Unknown audio position. Analyze again.")?;
    preview_clip(&path, at.unwrap(), CLIP_SECONDS)
}

fn start_analyze(req: AnalyzeReq, state: &Shared) -> Result<Value, String> {
    if !(req.min_silence > 0.0) {
        return Err("Minimum silence must be a positive number.".into());
    }
    if !req.noise_db.is_finite() {
        return Err("Noise threshold must be a number.".into());
    }
    if !(0.0..=1.0).contains(&req.threshold) {
        return Err("Match threshold must be between 0 and 1.".into());
    }
    let inputs = collect_inputs(&user_path(&req.input))?;
    if inputs.is_empty() {
        return Err("No supported audio files found in that folder.".into());
    }
    // Fail fast on a bad EPUB or missing model, before any slow work starts.
    let epub_path = user_path(&req.epub);
    let epub = if epub_path.as_os_str().is_empty() {
        None
    } else {
        Some((load_chapters(&epub_path)?, find_model()?))
    };
    let mut meta = Metadata { genre: "Audiobook".into(), template: DEFAULT_TEMPLATE.into(), ..Default::default() };
    if epub.is_some() {
        (meta.album, meta.artist, meta.year) = book_info(&epub_path)?;
    }

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
        s.log.push(format!("Analyzing {} file(s): min silence {}s, noise {}dB", inputs.len(), req.min_silence, req.noise_db));
        if let Some((chapters, _)) = &epub {
            s.log.push(format!("EPUB: {} chapters", chapters.len()));
        }
    }

    let state = state.clone();
    thread::spawn(move || {
        let mut files = Vec::new();
        for (i, path) in inputs.iter().enumerate() {
            let result = detect_silences(path, req.min_silence, req.noise_db);
            let mut s = state.lock().unwrap();
            match result {
                Ok((silences, duration)) => {
                    let count = split_points(&silences, req.min_silence, duration).len();
                    if epub.is_none() {
                        s.log.push(format!("{}: {} segments", file_name(path), count));
                    } else {
                        s.log.push(format!("{}: {} candidate silences", file_name(path), count.saturating_sub(1)));
                    }
                    files.push(AudioFile { path: path.clone(), silences, duration });
                }
                Err(e) => {
                    s.files[i].error = Some(e.clone());
                    s.log.push(format!("{}: {}", file_name(path), e));
                }
            }
            s.done = i + 1;
        }

        let result = match &epub {
            None => Ok((
                silence_breaks(&files, req.min_silence)
                    .into_iter()
                    .map(|b| BreakRow { file: b.file, at: b.at, title: b.title, score: None })
                    .collect(),
                candidates(&files, req.min_silence)
                    .iter()
                    .map(|c| SilenceRow { file: c.file, at: c.split_at(), before: String::new(), after: String::new() })
                    .collect(),
            )),
            Some((chapters, model)) => match_with_epub(&files, chapters, model, &req, &state),
        };

        let mut s = state.lock().unwrap();
        match result {
            Ok((breaks, silences)) => {
                s.breaks = breaks;
                s.silences = silences;
                s.audio = files.iter().map(|f| AudioRow { name: file_name(&f.path), duration: f.duration }).collect();
                s.analyzed = files;
                s.epub = epub.map(|(chapters, model)| {
                    Arc::new(EpubRun { chapters, model, min_silence: req.min_silence, noise_db: req.noise_db, threshold: req.threshold })
                });
                s.log.push("Analysis complete.".into());
            }
            Err(e) => s.log.push(format!("Error: {}", e)),
        }
        s.running = false;
    });
    Ok(json!({}))
}

// Transcribes around each candidate silence and matches it to the EPUB's chapters.
// Returns the suggested chapter breaks and every candidate with what was heard there.
fn match_with_epub(
    files: &[AudioFile],
    chapters: &[audiobook_splitter_rust::book::Chapter],
    model: &std::path::Path,
    req: &AnalyzeReq,
    state: &Shared,
) -> Result<(Vec<BreakRow>, Vec<SilenceRow>), String> {
    let cands = candidates(files, req.min_silence);
    {
        let mut s = state.lock().unwrap();
        s.stage = "Transcribing around silences".into();
        s.done = 0;
        s.total = cands.len();
        s.log.push(format!("Transcribing {} candidate breaks with Whisper...", cands.len()));
    }
    let transcriber = Transcriber::load(model)?;
    let transcripts = transcribe_candidates(files, &cands, &transcriber, |done, _| {
        state.lock().unwrap().done = done;
    })?;

    let m = pick_breaks(&cands, &transcripts, chapters, req.threshold);
    let breaks = chapter_breaks(&m, chapters)
        .into_iter()
        .map(|b| {
            let score = m.breaks.iter().find(|c| c.file == b.file && c.at == b.at).map(|c| c.score);
            BreakRow { file: b.file, at: b.at, title: b.title, score }
        })
        .collect();
    let silences = silence_rows(&cands, &transcripts);

    let mut s = state.lock().unwrap();
    s.missing = m.missing.iter().map(|&ch| MissingRow { chapter: ch, title: chapter_title(chapters, ch), tried: false }).collect();
    s.log.push(format!("Matched {} of {} chapters.", m.breaks.len(), chapters.len()));
    s.found = m.breaks;
    Ok((breaks, silences))
}

fn silence_rows(cands: &[Candidate], transcripts: &[(String, String)]) -> Vec<SilenceRow> {
    cands
        .iter()
        .zip(transcripts)
        .map(|(c, (before, after))| SilenceRow {
            file: c.file,
            at: c.split_at(),
            before: before.trim().to_string(),
            after: after.trim().to_string(),
        })
        .collect()
}

fn chapter_title(chapters: &[audiobook_splitter_rust::book::Chapter], ch: usize) -> String {
    chapters[ch].title.clone().unwrap_or_else(|| format!("Section {}", ch + 1))
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
        chapters.retain(|ch| s.missing.iter().any(|m| m.chapter == *ch));
        if chapters.is_empty() {
            return Err("Those chapters aren't missing any more.".into());
        }
        let files: Vec<AudioFile> =
            s.analyzed.iter().map(|f| AudioFile { path: f.path.clone(), silences: vec![], duration: f.duration }).collect();
        s.running = true;
        s.phase = "analyze";
        (epub, files, chapters)
    };
    chapters.sort();
    chapters.dedup();

    let state = state.clone();
    thread::spawn(move || {
        let result = Transcriber::load(&epub.model).and_then(|transcriber| {
            for ch in chapters {
                rescan_one(ch, &epub, &files, &transcriber, &state)?;
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

fn rescan_one(ch: usize, epub: &EpubRun, files: &[AudioFile], transcriber: &Transcriber, state: &Shared) -> Result<(), String> {
    let title = chapter_title(&epub.chapters, ch);
    let min_silence = epub.min_silence * RESCAN_SILENCE_FACTOR;
    let noise_db = epub.noise_db + RESCAN_NOISE_DB;
    let (from, to) = rescan_window(&state.lock().unwrap().found, ch, files);
    let ranges = window_ranges(files, from, to);
    {
        let mut s = state.lock().unwrap();
        s.stage = format!("Re-scanning '{}': detecting silences", title);
        s.done = 0;
        s.total = ranges.len();
        s.log.push(format!("Re-scanning '{}' (min silence {}s, noise {}dB)", title, min_silence, noise_db));
    }

    let mut scan: Vec<AudioFile> =
        files.iter().map(|f| AudioFile { path: f.path.clone(), silences: vec![], duration: f.duration }).collect();
    for (i, &(f, start, end)) in ranges.iter().enumerate() {
        scan[f].silences = detect_silences_between(&files[f].path, start, end, min_silence, noise_db)?;
        state.lock().unwrap().done = i + 1;
    }
    let cands = window_candidates(&scan, from, to, min_silence);
    {
        let mut s = state.lock().unwrap();
        s.stage = format!("Re-scanning '{}': transcribing", title);
        s.done = 0;
        s.total = cands.len();
    }
    let transcripts = transcribe_candidates(&scan, &cands, transcriber, |done, _| state.lock().unwrap().done = done)?;
    let found = rescan_chapter(&cands, &transcripts, &epub.chapters, ch, epub.threshold);

    let mut s = state.lock().unwrap();
    for row in silence_rows(&cands, &transcripts) {
        if !s.silences.iter().any(|x| x.file == row.file && (x.at - row.at).abs() < 0.001) {
            s.silences.push(row);
        }
    }
    s.silences.sort_by(|a, b| (a.file, a.at).partial_cmp(&(b.file, b.at)).unwrap());
    match found {
        Some(b) => {
            s.log.push(format!("Found '{}' (score {:.2}).", title, b.score));
            s.breaks.push(BreakRow { file: b.file, at: b.at, title, score: Some(b.score) });
            s.missing.retain(|m| m.chapter != ch);
            s.found.push(b);
        }
        None => {
            s.log.push(format!("'{}': no match among {} silences.", title, cands.len()));
            if let Some(m) = s.missing.iter_mut().find(|m| m.chapter == ch) {
                m.tried = true;
            }
        }
    }
    Ok(())
}

fn start_split(req: SplitReq, state: &Shared) -> Result<Value, String> {
    let output = user_path(&req.output);
    if output.as_os_str().is_empty() {
        return Err("Choose an output folder.".into());
    }
    if output.exists() && !output.is_dir() {
        return Err(format!("'{}' is not a folder.", output.display()));
    }

    let (plan, tracks) = {
        let mut s = state.lock().unwrap();
        if s.running {
            return Err("A job is already running.".into());
        }
        if s.analyzed.is_empty() {
            return Err("Run an analysis first.".into());
        }
        let plan = plan_from_breaks(&s.analyzed, &req.breaks)?;
        let tracks = tracks(&plan, &req.meta)?;
        std::fs::create_dir_all(&output)
            .map_err(|e| format!("Failed to create '{}': {}", output.display(), e))?;
        s.running = true;
        s.phase = "split";
        s.stage = "Writing files".into();
        s.done = 0;
        s.total = plan.len();
        s.log.push(format!("Splitting into {}", output.display()));
        (plan, tracks)
    };

    let state = state.clone();
    thread::spawn(move || {
        for (segment, track) in plan.iter().zip(&tracks) {
            let result = write_segment(&segment.pieces, &output, track);
            let mut s = state.lock().unwrap();
            s.done += 1;
            match result {
                Ok(path) => s.log.push(format!("Wrote {}", file_name(&path))),
                Err(e) => s.log.push(e),
            }
        }
        let mut s = state.lock().unwrap();
        s.done = s.total;
        s.running = false;
        s.log.push("Done.".into());
    });
    Ok(json!({}))
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_rejects_unknown_audio() {
        let state: Shared = Arc::default();
        assert!(clip(&state, "file=0&at=1").is_err());
        state.lock().unwrap().analyzed.push(AudioFile { path: "a.mp3".into(), silences: vec![], duration: 10.0 });
        for bad in ["file=1&at=1", "file=0&at=10", "file=0&at=-1", "file=0&at=NaN", "file=x&at=1", "at=1", ""] {
            assert!(clip(&state, bad).is_err(), "{}", bad);
        }
    }

    #[test]
    fn byte_range_parses_requests() {
        assert_eq!(byte_range(None, 100), None);
        assert_eq!(byte_range(Some("bytes=0-1"), 100), Some((0, 1)));
        assert_eq!(byte_range(Some("bytes=10-"), 100), Some((10, 99)));
        assert_eq!(byte_range(Some("bytes=50-500"), 100), Some((50, 99)));
        assert_eq!(byte_range(Some("bytes=100-"), 100), None);
        assert_eq!(byte_range(Some("bytes=9-3"), 100), None);
        assert_eq!(byte_range(Some("junk"), 100), None);
        assert_eq!(byte_range(Some("bytes=0-"), 0), None);
    }
}
