// The full-screen terminal UI: the chapter editor (used by `edit` and `run`) and the `tui` app.
pub mod browser;
pub mod editor;
pub mod help;
pub mod hints;
pub mod input;
pub mod jobs;
pub mod progress;
pub mod setup;
pub mod tags;
pub mod terminal;

use audiobook_splitter_rust::plan::{Findings, Plan};
use editor::{Editor, Outcome};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use terminal::TerminalGuard;
use super::{check_tools, open_cache, open_plan};
use audiobook_splitter_rust::pipeline::{self, analyze, chapter_title, default_metadata, file_name, open_epub, prepare_split, rescan_one, write_tracks, LazyTranscriber, SplitReport};
use audiobook_splitter_rust::user_path;
use browser::{Browser, BrowserOutcome, Pick};
use jobs::{Job, Msg};
use progress::Progress;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;
use setup::{AnalyzeRequest, Setup, SetupOutcome};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(clap::Args)]
pub struct Args {
    /// An audio file or folder to start with, or a plan file made by `analyze` to edit
    pub path: Option<PathBuf>,
}

struct DoneInfo {
    written: usize,
    failed: usize,
    out: PathBuf,
    plan_path: Option<PathBuf>,
    error: Option<String>,
    plan_error: Option<String>,
    log: Vec<String>,
}

enum Screen {
    Setup,
    Analyzing { progress: Progress, job: Job<Plan> },
    Editor(Box<Editor>),
    PickOutput { editor: Box<Editor>, browser: Browser },
    Splitting { progress: Progress, job: Job<SplitReport> },
    Rescanning { editor: Box<Editor>, progress: Progress, job: Job<RescanResult> },
    Done(DoneInfo),
}

// What a re-scan job hands back: the findings with every result folded in, how many missing
// chapters it looked for and found, and why it stopped early, if it did.
pub struct RescanResult {
    findings: Findings,
    asked: usize,
    found: usize,
    error: Option<String>,
}

// Re-scans each of `chapters` (sorted, no repeats, only those still missing) one at a time. Each
// result is folded into the findings before the next chapter, so the next search window uses the
// updated `found` list. An error stops the loop but keeps what was found so far.
fn run_rescan(
    mut chapters: Vec<usize>,
    mut findings: Findings,
    title: impl Fn(usize) -> String,
    mut rescan: impl FnMut(usize, &Findings) -> Result<pipeline::Rescan, String>,
) -> RescanResult {
    chapters.sort_unstable();
    chapters.dedup();
    chapters.retain(|c| findings.missing.iter().any(|m| m.chapter == *c));
    let (asked, mut found, mut error) = (chapters.len(), 0, None);
    for ch in chapters {
        match rescan(ch, &findings) {
            Ok(r) => {
                found += r.found.is_some() as usize;
                findings.apply_rescan(ch, title(ch), r.found, r.silences);
            }
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }
    RescanResult { findings, asked, found, error }
}

// Built once per job and moved straight on, so the size difference costs nothing.
#[allow(clippy::large_enum_variant)]
enum Finished {
    Analysis(Result<Plan, String>),
    Split(Result<SplitReport, String>),
    Rescan(Result<RescanResult, String>),
}

// The whole terminal app: one screen at a time, the Setup form kept across screens so `b`
// (back) finds what the user typed.
pub struct App {
    screen: Screen,
    setup: Setup,
    out: Option<PathBuf>,
    plan_path: Option<PathBuf>,
    confirm_quit: bool,
    help: bool,
    quit: bool,
    // Why the plan could not be saved when the split started; shown on the Done screen.
    plan_error: Option<String>,
    // Whether the cache is on (the Setup choice), for re-scans started from the editor.
    use_cache: bool,
}

fn is_plan_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

// Everything a running job has sent so far; the job's result if it has finished.
fn drain<T>(job: &Job<T>, progress: &mut Progress) -> Option<Result<T, String>> {
    while let Some(msg) = job.try_next() {
        match msg {
            Msg::Event(e) => progress.apply(e),
            Msg::Done(r) => return Some(r),
        }
    }
    None
}

impl App {
    pub fn new(arg: Option<&Path>) -> Result<App, String> {
        match arg {
            Some(path) if is_plan_path(path) => Ok(App::with_plan(open_plan(path)?, Some(path.to_path_buf()))),
            other => Ok(App {
                screen: Screen::Setup,
                setup: Setup::new(other.map(|p| p.display().to_string()).as_deref()),
                out: None,
                plan_path: None,
                confirm_quit: false,
                help: false,
                quit: false,
                plan_error: None,
                use_cache: true,
            }),
        }
    }

    // An app that starts in the editor on an existing plan.
    pub fn with_plan(plan: Plan, plan_path: Option<PathBuf>) -> App {
        let mut editor = Editor::new(plan);
        editor.allow_back = true;
        App { screen: Screen::Editor(Box::new(editor)), setup: Setup::new(None), out: None, plan_path, confirm_quit: false, help: false, quit: false, plan_error: None, use_cache: true }
    }

    pub fn is_over(&self) -> bool {
        self.quit
    }

    // Fails fast (bad EPUB, missing tools) before any slow work starts, then runs the analysis
    // on a worker thread and shows its progress.
    fn start_analysis(&mut self, req: AnalyzeRequest) -> Result<(), String> {
        check_tools()?;
        let (epub, meta) = match &req.epub {
            Some(path) => {
                let (epub, info) = open_epub(path)?;
                (Some(epub), default_metadata(Some(info)))
            }
            None => (None, default_metadata(None)),
        };
        let cache = open_cache(!req.use_cache);
        self.use_cache = req.use_cache;
        let AnalyzeRequest { inputs, epub: epub_path, out, settings, .. } = req;
        self.plan_path = Some(out.join("plan.json"));
        self.out = Some(out);
        let job = Job::spawn(move |emit| {
            let analysis = analyze(&inputs, &settings, epub.as_ref(), &cache, emit)?;
            for (i, why) in &analysis.errors {
                emit(pipeline::Event::Log(format!("Skipped {}: {}", file_name(&inputs[*i]), why)));
            }
            if analysis.files.is_empty() {
                return Err("None of the audio files could be analyzed.".to_string());
            }
            Ok(Plan::new(analysis.files, settings, epub_path, analysis.findings, meta))
        });
        self.screen = Screen::Analyzing { progress: Progress::new("Analyzing"), job };
        Ok(())
    }

    // Fails fast (missing tools, unreadable EPUB) back in the editor, then re-scans the missing
    // `chapters` one by one on a worker thread.
    fn start_rescan(&mut self, mut editor: Box<Editor>, chapters: Vec<usize>) {
        let prepared = check_tools().and_then(|_| match editor.plan.epub.clone() {
            Some(path) => open_epub(&path).map(|(epub, _)| epub),
            None => Err("This plan was made without an EPUB, so there is nothing to re-scan against.".to_string()),
        });
        let epub = match prepared {
            Ok(epub) => epub,
            Err(e) => {
                editor.set_status(e);
                self.screen = Screen::Editor(editor);
                return;
            }
        };
        let (files, findings, settings) = (editor.plan.files.clone(), editor.plan.findings.clone(), editor.plan.settings.clone());
        let cache = open_cache(!self.use_cache);
        let job = Job::spawn(move |emit| {
            let mut transcriber = LazyTranscriber::new(&epub.model);
            Ok(run_rescan(
                chapters,
                findings,
                |ch| chapter_title(&epub.chapters, ch),
                |ch, f| rescan_one(ch, &files, &f.found, &settings, &epub, &mut transcriber, &cache, emit),
            ))
        });
        self.screen = Screen::Rescanning { editor, progress: Progress::new("Re-scanning"), job };
    }

    // Validates the split before anything is written, saves the plan, then writes the files on a worker thread.
    fn start_split(&mut self, mut editor: Box<Editor>, out: PathBuf) {
        let plan = editor.plan.clone();
        let prepared = prepare_split(&plan.files, &plan.findings.to_breaks(), &plan.metadata)
            .and_then(|p| std::fs::create_dir_all(&out).map(|_| p).map_err(|e| format!("Failed to create '{}': {}", out.display(), e)));
        let (segments, names) = match prepared {
            Ok(p) => p,
            Err(e) => {
                editor.set_status(e);
                self.screen = Screen::Editor(editor);
                return;
            }
        };
        self.plan_error = None;
        if let Some(path) = &self.plan_path
            && let Err(e) = plan.save(path)
        {
            self.plan_error = Some(e.to_string());
        }
        self.out = Some(out.clone());
        let (files, cover) = (plan.files.clone(), plan.metadata.cover.clone());
        let job = Job::spawn(move |emit| Ok(write_tracks(&segments, &names, &out, &files, &cover, emit)));
        self.screen = Screen::Splitting { progress: Progress::new("Splitting"), job };
    }

    // Moves finished jobs on to the next screen. Called every frame.
    pub fn tick(&mut self) {
        let finished = match &mut self.screen {
            Screen::Analyzing { progress, job } => drain(job, progress).map(Finished::Analysis),
            Screen::Splitting { progress, job } => drain(job, progress).map(Finished::Split),
            Screen::Rescanning { progress, job, .. } => drain(job, progress).map(Finished::Rescan),
            _ => None,
        };
        match finished {
            Some(Finished::Analysis(Ok(plan))) => {
                let mut notice = None;
                if let Some(path) = &self.plan_path {
                    let saved = path.parent().map_or(Ok(()), std::fs::create_dir_all).map_err(|e| e.to_string()).and_then(|_| plan.save(path));
                    if let Err(e) = saved {
                        notice = Some(format!("Couldn't save the plan: {}", e));
                    }
                }
                let mut editor = Editor::new(plan);
                editor.allow_back = true;
                if let Some(n) = notice {
                    editor.set_status(n);
                }
                self.confirm_quit = false;
                self.help = false;
                self.screen = Screen::Editor(Box::new(editor));
            }
            Some(Finished::Analysis(Err(e))) => {
                self.setup.error = e;
                self.confirm_quit = false;
                self.help = false;
                self.screen = Screen::Setup;
            }
            Some(Finished::Split(result)) => {
                let out = self.out.clone().unwrap_or_default();
                let log = match &self.screen {
                    Screen::Splitting { progress, .. } => progress.tail(8),
                    _ => Vec::new(),
                };
                let (plan_path, plan_error) = (self.plan_path.clone(), self.plan_error.take());
                let info = match result {
                    Ok(report) => DoneInfo { written: report.written.len(), failed: report.failed, out, plan_path, error: None, plan_error, log },
                    Err(e) => DoneInfo { written: 0, failed: 0, out, plan_path, error: Some(e), plan_error, log },
                };
                self.confirm_quit = false;
                self.help = false;
                self.screen = Screen::Done(info);
            }
            Some(Finished::Rescan(result)) => {
                if let Screen::Rescanning { mut editor, .. } = std::mem::replace(&mut self.screen, Screen::Setup) {
                    match result {
                        Ok(r) => editor.apply_rescan(r.findings, r.asked, r.found, r.error),
                        Err(e) => editor.set_status(format!("The re-scan failed: {}", e)),
                    }
                    self.confirm_quit = false;
                    self.help = false;
                    self.screen = Screen::Editor(editor);
                }
            }
            None => {}
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.confirm_quit {
            self.confirm_quit = false;
            if key.code == KeyCode::Char('y') {
                self.quit = true;
            }
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        if key.code == KeyCode::Char('?') && matches!(self.screen, Screen::Analyzing { .. } | Screen::Splitting { .. } | Screen::Rescanning { .. } | Screen::Done(_)) {
            self.help = true;
            return;
        }
        let ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        match std::mem::replace(&mut self.screen, Screen::Setup) {
            Screen::Setup => match self.setup.handle_key(key) {
                SetupOutcome::Continue => {}
                SetupOutcome::Quit => self.quit = true,
                SetupOutcome::Start(req) => {
                    if let Err(e) = self.start_analysis(req) {
                        self.setup.error = e;
                    }
                }
            },
            screen @ (Screen::Analyzing { .. } | Screen::Splitting { .. } | Screen::Rescanning { .. }) => {
                self.screen = screen;
                if ctrl_c || key.code == KeyCode::Char('q') {
                    self.confirm_quit = true;
                }
            }
            Screen::Editor(mut editor) => match editor.handle_key(key) {
                Outcome::Continue => self.screen = Screen::Editor(editor),
                Outcome::Discard => self.quit = true,
                Outcome::Back => self.screen = Screen::Setup,
                Outcome::Rescan(chapters) => self.start_rescan(editor, chapters),
                Outcome::Save => match self.out.clone() {
                    Some(out) => self.start_split(editor, out),
                    None => {
                        let browser = Browser::new("Choose the output folder", "", Pick::Folder);
                        self.screen = Screen::PickOutput { editor, browser };
                    }
                },
            },
            Screen::PickOutput { editor, mut browser } => match browser.handle_key(key) {
                BrowserOutcome::Continue => self.screen = Screen::PickOutput { editor, browser },
                BrowserOutcome::Cancel => self.screen = Screen::Editor(editor),
                BrowserOutcome::Chosen(out) => self.start_split(editor, out),
            },
            Screen::Done(info) => {
                if key.code == KeyCode::Char('n') {
                    self.screen = Screen::Setup;
                } else if ctrl_c || matches!(key.code, KeyCode::Char('q') | KeyCode::Enter | KeyCode::Esc) {
                    self.quit = true;
                } else {
                    self.screen = Screen::Done(info);
                }
            }
        }
    }

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        match &mut self.screen {
            Screen::Setup => self.setup.render(f, area),
            Screen::Analyzing { progress, .. } | Screen::Splitting { progress, .. } | Screen::Rescanning { progress, .. } => progress.render(f, area),
            Screen::Editor(editor) => editor.render(f),
            Screen::PickOutput { editor, browser } => {
                editor.render(f);
                browser.render(f, area);
            }
            Screen::Done(info) => render_done(f, area, info),
        }
        if self.confirm_quit {
            editor::confirm(f, area, " Quit ", Some("Job running. Quit anyway?"), &[("y", "quit"), ("other key:", "stay")], "  ");
        }
        if self.help {
            match &self.screen {
                Screen::Done(_) => help::render_help(f, area, "Keys", help::DONE_KEYS),
                _ => help::render_help(f, area, "Keys", help::PROGRESS_KEYS),
            }
        }
    }
}

fn render_done(f: &mut Frame, area: Rect, info: &DoneInfo) {
    let mut lines = Vec::new();
    match &info.error {
        Some(e) => lines.push(Line::raw(format!("The split failed: {}", e))),
        None => {
            lines.push(Line::raw(format!("Wrote {} file(s) to {}.", info.written, info.out.display())));
            if info.failed > 0 {
                let retry = match &info.plan_path {
                    Some(p) if info.plan_error.is_none() => format!("split {}", p.display()),
                    _ => "split".to_string(),
                };
                lines.push(Line::raw(format!("{} file(s) could not be written; the log is below. You can retry with `{}`.", info.failed, retry)));
            }
        }
    }
    match (&info.plan_error, &info.plan_path) {
        (Some(e), _) => lines.push(Line::raw(format!("The plan could not be saved: {}", e))),
        (None, Some(p)) => lines.push(Line::raw(format!("The plan is saved at {}.", p.display()))),
        (None, None) => {}
    }
    lines.push(Line::raw(""));
    lines.extend(hints::wrap_hints(&[("n", "another book"), ("q/Enter", "quit"), ("?", "help")], "    ", area.width.saturating_sub(2) as usize));
    if info.failed > 0 || info.error.is_some() {
        lines.push(Line::raw(""));
        lines.extend(info.log.iter().map(|l| Line::raw(l.as_str())));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(Block::bordered().title(" Done ")), area);
}

// Only a panic on the UI thread may restore the terminal; the app is still drawing otherwise.
fn should_restore(ui: std::thread::ThreadId, current: std::thread::ThreadId) -> bool {
    ui == current
}

// `tui`: the full-screen app, from choosing the book to splitting it.
pub fn run(args: Args) -> Result<(), String> {
    if !terminal::suitable(false) {
        return Err("`tui` needs an interactive terminal at least 60 columns by 15 rows with TERM set. Use `run`, or `analyze`, `edit` and `split` step by step, instead.".into());
    }
    check_tools()?;
    let mut app = App::new(args.path.as_deref().map(|p| user_path(&p.to_string_lossy())).as_deref())?;
    let mut guard = TerminalGuard::new()?;
    // `ratatui::try_init` made a panic hook that restores the terminal. A panic on any other
    // thread (a job, or workers it starts) is caught and shown as an error, so that hook
    // must run only for the thread that owns the screen.
    let ui = std::thread::current().id();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if should_restore(ui, std::thread::current().id()) {
            previous(info);
        }
    }));
    while !app.is_over() {
        guard.terminal.draw(|f| app.render(f)).map_err(|e| e.to_string())?;
        if event::poll(Duration::from_millis(100)).map_err(|e| e.to_string())?
            && let Event::Key(key) = event::read().map_err(|e| e.to_string())?
            && key.kind == KeyEventKind::Press
        {
            app.handle_key(key);
        }
        app.tick();
    }
    Ok(())
}

// Runs the full-screen chapter editor on `plan`. Returns whether to save; on true `*plan`
// holds the edited plan. The terminal is restored before this returns.
pub fn edit(plan: &mut Plan, mut guard: TerminalGuard) -> Result<bool, String> {
    let mut ed = Editor::new(plan.clone());
    let outcome = loop {
        guard.terminal.draw(|f| ed.render(f)).map_err(|e| e.to_string())?;
        match event::read().map_err(|e| e.to_string())? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match ed.handle_key(key) {
                Outcome::Continue | Outcome::Back | Outcome::Rescan(_) => {}
                done => break done,
            },
            _ => {} // Resize and the rest: just redraw.
        }
    };
    drop(guard);
    match outcome {
        Outcome::Save => {
            *plan = ed.plan;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::edit::ops::sample_plan;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::Terminal;
    use std::time::Duration;

    #[test]
    fn only_the_ui_thread_restores_the_terminal_on_a_panic() {
        let ui = std::thread::current().id();
        assert!(should_restore(ui, std::thread::current().id()));
        let other = std::thread::spawn(move || should_restore(ui, std::thread::current().id())).join().unwrap();
        assert!(!other, "an unnamed worker thread must not restore the terminal");
    }

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        k(KeyCode::Char(c))
    }

    // A 15 s WAV with two silences (4-6 s and 9-11 s) in a fresh folder: three chapters.
    fn book(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("abs-app-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("book.wav");
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "aevalsrc=if(between(t\\,4\\,6)+between(t\\,9\\,11)\\,0\\,sin(2*PI*440*t)):d=15", "-y"])
            .arg(&wav)
            .status()
            .unwrap();
        assert!(made.success());
        (dir, wav)
    }

    fn request(wav: &Path, out: &Path) -> setup::AnalyzeRequest {
        setup::AnalyzeRequest {
            inputs: vec![wav.to_path_buf()],
            epub: None,
            out: out.to_path_buf(),
            settings: audiobook_splitter_rust::plan::Settings { min_silence: 1.0, ..Default::default() },
            use_cache: false,
        }
    }

    fn wait(app: &mut App, what: &str, until: impl Fn(&App) -> bool) {
        for _ in 0..600 {
            app.tick();
            if until(app) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for {}", what);
    }

    fn in_editor(app: &App) -> bool {
        matches!(app.screen, Screen::Editor(_))
    }

    fn draw(app: &mut App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| app.render(f)).unwrap();
        let buf = t.backend().buffer();
        (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn the_whole_flow_analyzes_edits_splits_and_finishes() {
        let (dir, wav) = book("flow");
        let out = dir.join("out");
        let mut app = App::new(Some(wav.as_path())).unwrap();
        assert!(matches!(app.screen, Screen::Setup));
        app.start_analysis(request(&wav, &out)).unwrap();
        assert!(matches!(app.screen, Screen::Analyzing { .. }));
        wait(&mut app, "the analysis", in_editor);
        let Screen::Editor(ed) = &app.screen else { unreachable!() };
        assert_eq!(ed.plan.findings.breaks.iter().map(|b| b.title.as_str()).collect::<Vec<_>>(), ["Part 1", "Part 2", "Part 3"]);
        assert!(out.join("plan.json").is_file(), "the plan is saved beside the output, as `run` does");

        app.handle_key(ch('s'));
        assert!(matches!(app.screen, Screen::Splitting { .. }));
        wait(&mut app, "the split", |a| matches!(a.screen, Screen::Done(_)));
        let Screen::Done(info) = &app.screen else { unreachable!() };
        assert_eq!((info.written, info.failed), (3, 0));
        let mut files: Vec<String> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".wav")).collect();
        files.sort();
        assert_eq!(files, ["00 - Part 1.wav", "01 - Part 2.wav", "02 - Part 3.wav"]);

        let screen = draw(&mut app, 80, 24);
        assert!(screen.contains("Wrote 3 file(s)"), "{}", screen);
        app.handle_key(ch('n'));
        assert!(matches!(app.screen, Screen::Setup), "another book returns to Setup");
        assert!(!app.is_over());
    }

    #[test]
    fn question_mark_opens_help_on_the_progress_and_done_screens() {
        let mut app = App::new(None).unwrap();
        let (_tx, rx) = std::sync::mpsc::channel();
        app.screen = Screen::Analyzing { progress: progress::Progress::new("Analyzing"), job: jobs::Job::from_receiver(rx) };
        assert!(draw(&mut app, 80, 24).contains("? help"), "the hint names the key");
        app.handle_key(ch('?'));
        assert!(draw(&mut app, 80, 24).contains("Ctrl-c"));
        app.handle_key(ch('x'));
        assert!(!app.is_over() && matches!(app.screen, Screen::Analyzing { .. }), "the closing key does nothing else");
        app.screen = Screen::Done(DoneInfo { written: 3, failed: 0, out: "/tmp/out".into(), plan_path: None, error: None, plan_error: None, log: Vec::new() });
        assert!(draw(&mut app, 80, 24).contains("? help"));
        app.handle_key(ch('?'));
        assert!(draw(&mut app, 80, 24).contains("back to Setup"), "only the help popup says this");
        app.handle_key(ch('x'));
        assert!(matches!(app.screen, Screen::Done(_)));
    }

    #[test]
    fn popups_draw_at_small_sizes_on_every_screen() {
        let mut plan = sample_plan();
        plan.findings.breaks[1].title = "A very long title ".repeat(10);
        let mut app = App::with_plan(plan, None);
        let sizes = [(80, 24), (60, 15), (40, 8), (20, 5)];
        for keys in ["", "r", "\u{1b}n", "\u{1b}?", "\u{1b}\u{1b}t"] {
            for c in keys.chars() {
                app.handle_key(if c == '\u{1b}' { k(KeyCode::Esc) } else { ch(c) });
            }
            for (w, h) in sizes {
                draw(&mut app, w, h);
            }
        }
        app.handle_key(k(KeyCode::Esc));
        app.handle_key(ch('x'));
        app.handle_key(ch('s')); // no output folder: the browser opens
        for (w, h) in sizes {
            draw(&mut app, w, h);
        }
        let mut setup = App::new(None).unwrap();
        setup.handle_key(k(KeyCode::Enter)); // the Setup file browser
        setup.handle_key(k(KeyCode::Tab)); // typing a path
        for (w, h) in sizes {
            draw(&mut setup, w, h);
        }
        setup.handle_key(k(KeyCode::Esc)); // leaves typing
        setup.handle_key(k(KeyCode::Esc)); // closes the browser
        setup.handle_key(k(KeyCode::F(1)));
        for (w, h) in sizes {
            draw(&mut setup, w, h);
        }
        app.screen = Screen::Done(DoneInfo { written: 1, failed: 2, out: "/a/very/long/output/folder/name".into(), plan_path: Some("/p/plan.json".into()), error: Some("boom".into()), plan_error: None, log: Vec::new() });
        for (w, h) in sizes {
            draw(&mut app, w, h);
        }
        app.handle_key(ch('?'));
        for (w, h) in sizes {
            draw(&mut app, w, h);
        }
    }

    #[test]
    fn back_returns_to_setup_with_the_values_kept() {
        let (dir, wav) = book("back");
        let mut app = App::new(Some(wav.as_path())).unwrap();
        app.start_analysis(request(&wav, &dir.join("out"))).unwrap();
        wait(&mut app, "the analysis", in_editor);
        app.handle_key(ch('b'));
        assert!(matches!(app.screen, Screen::Setup));
        let screen = draw(&mut app, 80, 24);
        assert!(screen.contains("book.wav"), "the input is still filled in:\n{}", screen);
    }

    #[test]
    fn quitting_the_editor_without_changes_ends_the_app() {
        let mut app = App::with_plan(sample_plan(), None);
        assert!(matches!(app.screen, Screen::Editor(_)));
        app.handle_key(ch('q'));
        assert!(app.is_over());
    }

    #[test]
    fn saving_a_plan_with_no_output_folder_asks_for_one_and_esc_returns() {
        let mut app = App::with_plan(sample_plan(), None);
        app.handle_key(ch('s'));
        assert!(matches!(app.screen, Screen::PickOutput { .. }));
        assert!(draw(&mut app, 80, 24).contains("Choose the output folder"));
        app.handle_key(k(KeyCode::Esc));
        assert!(in_editor(&app), "cancelling the browser returns to the editor");
    }

    #[test]
    fn a_plan_opened_directly_splits_into_the_chosen_folder() {
        let (dir, wav) = book("plan");
        let mut first = App::new(Some(wav.as_path())).unwrap();
        first.start_analysis(request(&wav, &dir.join("analysis"))).unwrap();
        wait(&mut first, "the analysis", in_editor);
        let Screen::Editor(ed) = first.screen else { unreachable!() };
        let out = dir.join("chosen");
        std::fs::create_dir_all(&out).unwrap();
        let mut app = App::with_plan(ed.plan.clone(), Some(dir.join("analysis").join("plan.json")));
        app.handle_key(ch('s'));
        let Screen::PickOutput { browser, .. } = &mut app.screen else { panic!("expected the output picker") };
        *browser = browser::Browser::new("Choose the output folder", out.to_str().unwrap(), browser::Pick::Folder);
        app.handle_key(ch(' '));
        assert!(matches!(app.screen, Screen::Splitting { .. }));
        wait(&mut app, "the split", |a| matches!(a.screen, Screen::Done(_)));
        assert_eq!(std::fs::read_dir(&out).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".wav")).count(), 3);
    }

    #[test]
    fn a_bad_template_sends_the_user_back_to_the_editor_with_the_reason() {
        let mut plan = sample_plan();
        plan.metadata.template = String::new();
        let mut app = App::with_plan(plan, None);
        app.out = Some(std::env::temp_dir().join(format!("abs-app-{}-badtemplate", std::process::id())));
        app.handle_key(ch('s'));
        assert!(in_editor(&app));
        assert!(draw(&mut app, 80, 24).contains("naming convention"));
    }

    #[test]
    fn a_failed_analysis_returns_to_setup_with_the_message() {
        let mut app = App::new(None).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        app.screen = Screen::Analyzing { progress: progress::Progress::new("Analyzing"), job: jobs::Job::from_receiver(rx) };
        tx.send(jobs::Msg::Done(Err("None of the audio files could be analyzed.".into()))).unwrap();
        app.tick();
        assert!(matches!(app.screen, Screen::Setup));
        assert!(draw(&mut app, 80, 24).contains("could be analyzed"));
    }

    #[test]
    fn ctrl_c_during_a_job_asks_before_quitting() {
        let mut app = App::new(None).unwrap();
        let (_tx, rx) = std::sync::mpsc::channel();
        app.screen = Screen::Analyzing { progress: progress::Progress::new("Analyzing"), job: jobs::Job::from_receiver(rx) };
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!app.is_over());
        assert!(draw(&mut app, 80, 24).contains("Quit"));
        app.handle_key(ch('n'));
        assert!(!app.is_over());
        app.handle_key(ch('q'));
        app.handle_key(ch('y'));
        assert!(app.is_over());
    }

    #[test]
    fn every_screen_draws_at_the_minimum_size_and_smaller() {
        let (dir, wav) = book("sizes");
        let mut app = App::new(Some(wav.as_path())).unwrap();
        for (w, h) in [(80, 24), (60, 15), (40, 8)] {
            draw(&mut app, w, h);
        }
        app.start_analysis(request(&wav, &dir.join("out"))).unwrap();
        for (w, h) in [(80, 24), (60, 15), (40, 8)] {
            draw(&mut app, w, h);
        }
        wait(&mut app, "the analysis", in_editor);
        for (w, h) in [(80, 24), (60, 15), (40, 8)] {
            draw(&mut app, w, h);
        }
    }

    #[test]
    fn a_broken_or_missing_plan_file_is_reported_not_opened() {
        let dir = std::env::temp_dir().join(format!("abs-app-{}-badplan", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "not a plan").unwrap();
        assert!(App::new(Some(bad.as_path())).is_err());
        assert!(App::new(Some(dir.join("missing.json").as_path())).is_err());
    }

    fn done_with(failed: usize, error: Option<String>, plan_error: Option<String>) -> DoneInfo {
        DoneInfo {
            written: 1,
            failed,
            out: PathBuf::from("/tmp/out"),
            plan_path: Some(PathBuf::from("/tmp/out/plan.json")),
            error,
            plan_error,
            log: vec!["first".into(), "boom line".into()],
        }
    }

    #[test]
    fn done_with_failures_shows_the_count_the_retry_hint_and_the_log() {
        let mut app = App::new(None).unwrap();
        app.screen = Screen::Done(done_with(2, None, None));
        let screen = draw(&mut app, 120, 24);
        assert!(screen.contains("2 file(s) could not be written"), "{}", screen);
        assert!(screen.contains("split /tmp/out/plan.json"), "{}", screen);
        assert!(screen.contains("boom line"), "{}", screen);
        for (w, h) in [(60, 15), (40, 8)] {
            draw(&mut app, w, h);
        }
        app.screen = Screen::Done(done_with(0, Some("ffmpeg died".into()), None));
        let screen = draw(&mut app, 80, 24);
        assert!(screen.contains("ffmpeg died") && screen.contains("boom line"), "{}", screen);
        for (w, h) in [(60, 15), (40, 8)] {
            draw(&mut app, w, h);
        }
    }

    #[test]
    fn a_plan_that_could_not_be_saved_is_reported_on_the_done_screen() {
        let (dir, wav) = book("plansave");
        let out = dir.join("out");
        let mut app = App::new(Some(wav.as_path())).unwrap();
        app.start_analysis(request(&wav, &out)).unwrap();
        wait(&mut app, "the analysis", in_editor);
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "a file").unwrap();
        app.plan_path = Some(blocker.join("plan.json"));
        app.handle_key(ch('s'));
        wait(&mut app, "the split", |a| matches!(a.screen, Screen::Done(_)));
        let Screen::Done(info) = &app.screen else { unreachable!() };
        assert_eq!((info.written, info.failed), (3, 0));
        let screen = draw(&mut app, 100, 24);
        assert!(screen.contains("could not be saved"), "{}", screen);
        assert!(!screen.contains("The plan is saved"), "{}", screen);
    }

    #[test]
    fn the_quit_popup_is_readable_at_60x15() {
        let mut app = App::new(None).unwrap();
        let (_tx, rx) = std::sync::mpsc::channel::<jobs::Msg<Plan>>();
        app.screen = Screen::Analyzing { progress: progress::Progress::new("Analyzing"), job: jobs::Job::from_receiver(rx) };
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        let screen = draw(&mut app, 60, 15);
        assert!(screen.contains("other key: stay"), "{}", screen);
    }

    #[test]
    fn a_job_finishing_closes_the_quit_prompt() {
        let mut app = App::new(None).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        app.screen = Screen::Analyzing { progress: progress::Progress::new("Analyzing"), job: jobs::Job::from_receiver(rx) };
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.confirm_quit);
        tx.send(jobs::Msg::Done(Err("nope".into()))).unwrap();
        app.tick();
        assert!(!app.confirm_quit);
        assert!(matches!(app.screen, Screen::Setup));
        app.handle_key(ch('y'));
        assert!(!app.is_over());
    }

    #[test]
    fn a_job_finishing_closes_the_help_popup() {
        let mut app = App::new(None).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        app.screen = Screen::Analyzing { progress: progress::Progress::new("Analyzing"), job: jobs::Job::from_receiver(rx) };
        app.handle_key(ch('?'));
        assert!(app.help);
        assert!(draw(&mut app, 100, 24).contains("Keys"), "the popup is drawn");
        tx.send(jobs::Msg::Done(Err("nope".into()))).unwrap();
        app.tick();
        assert!(!app.help);
        assert!(matches!(app.screen, Screen::Setup));
        app.handle_key(ch('z'));
        app.handle_key(ch('q'));
        let screen = draw(&mut app, 100, 24);
        assert!(screen.contains("zq"), "the next key is not swallowed:\n{}", screen);
    }

    #[test]
    fn a_rename_in_a_plan_opened_directly_is_saved_back_to_that_plan() {
        let (dir, wav) = book("rename");
        let mut first = App::new(Some(wav.as_path())).unwrap();
        first.start_analysis(request(&wav, &dir.join("analysis"))).unwrap();
        wait(&mut first, "the analysis", in_editor);
        let plan_path = dir.join("analysis").join("plan.json");
        let mut app = App::new(Some(plan_path.as_path())).unwrap();
        app.handle_key(ch('r'));
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        for c in "Renamed".chars() {
            app.handle_key(ch(c));
        }
        app.handle_key(k(KeyCode::Enter));
        let out = dir.join("chosen");
        std::fs::create_dir_all(&out).unwrap();
        app.handle_key(ch('s'));
        let Screen::PickOutput { browser, .. } = &mut app.screen else { panic!("expected the output picker") };
        *browser = browser::Browser::new("Choose the output folder", out.to_str().unwrap(), browser::Pick::Folder);
        app.handle_key(ch(' '));
        wait(&mut app, "the split", |a| matches!(a.screen, Screen::Done(_)));
        let saved = Plan::load(&plan_path).unwrap();
        assert!(saved.findings.breaks.iter().any(|b| b.title == "Renamed"), "{:?}", saved.findings.breaks.iter().map(|b| &b.title).collect::<Vec<_>>());
    }

    #[test]
    fn a_json_argument_is_a_plan_and_anything_else_a_book() {
        assert!(is_plan_path(Path::new("plan.json")) && is_plan_path(Path::new("/x/Plan.JSON")));
        assert!(!is_plan_path(Path::new("book.mp3")) && !is_plan_path(Path::new("/books/folder")));
    }

    use audiobook_splitter_rust::chapters::ChapterBreak;
    use audiobook_splitter_rust::plan::{Findings, Missing};

    fn hit(chapter: usize, at: f64) -> ChapterBreak {
        ChapterBreak { file: 0, at, chapter, score: 1.5, before: String::new(), after: String::new() }
    }

    fn missing_findings(chapters: &[usize]) -> Findings {
        let mut f = sample_plan().findings;
        f.found = vec![hit(0, 0.0), hit(1, 30.0), hit(2, 60.0)];
        f.missing = chapters.iter().map(|&c| Missing { chapter: c, title: format!("Ch {}", c), tried: false }).collect();
        f
    }

    #[test]
    fn run_rescan_goes_through_the_chapters_in_order_and_each_sees_the_earlier_finds() {
        let calls = std::cell::RefCell::new(Vec::new());
        let result = run_rescan(vec![5, 3, 5, 9, 4], missing_findings(&[3, 4, 5]), |c| format!("Ch {}", c), |ch, f| {
            calls.borrow_mut().push((ch, f.found.len()));
            Ok(pipeline::Rescan { found: (ch != 4).then(|| hit(ch, 40.0 + ch as f64)), silences: vec![] })
        });
        assert_eq!(*calls.borrow(), [(3, 3), (4, 4), (5, 4)], "sorted, deduplicated, never the non-missing 9; `found` grows after a hit");
        assert_eq!((result.asked, result.found, result.error), (3, 2, None));
        assert_eq!(result.findings.missing.iter().map(|m| (m.chapter, m.tried)).collect::<Vec<_>>(), [(4, true)]);
        assert_eq!(result.findings.breaks.iter().filter(|b| b.title.starts_with("Ch ")).count(), 2);
    }

    #[test]
    fn run_rescan_keeps_the_first_result_when_the_second_chapter_errors() {
        let result = run_rescan(vec![3, 4, 5], missing_findings(&[3, 4, 5]), |c| format!("Ch {}", c), |ch, _| {
            if ch == 4 { Err("whisper broke".into()) } else { Ok(pipeline::Rescan { found: Some(hit(ch, 45.0)), silences: vec![] }) }
        });
        assert_eq!((result.asked, result.found), (3, 1));
        assert_eq!(result.error.as_deref(), Some("whisper broke"));
        assert_eq!(result.findings.missing.iter().map(|m| m.chapter).collect::<Vec<_>>(), [4, 5]);
        assert!(result.findings.breaks.iter().any(|b| b.title == "Ch 3"));
    }

    fn rescan_editor() -> Editor {
        let mut p = sample_plan();
        p.epub = Some("book.epub".into());
        p.findings = missing_findings(&[3, 4]);
        let mut ed = Editor::new(p);
        ed.allow_back = true;
        ed
    }

    fn rescanning_app(ed: Editor) -> (App, std::sync::mpsc::Sender<jobs::Msg<RescanResult>>) {
        let mut app = App::new(None).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        app.screen = Screen::Rescanning { editor: Box::new(ed), progress: progress::Progress::new("Re-scanning"), job: jobs::Job::from_receiver(rx) };
        (app, tx)
    }

    #[test]
    fn a_finished_rescan_returns_to_the_editor_with_the_new_chapter_selected() {
        let ed = rescan_editor();
        let result = run_rescan(vec![3, 4], ed.plan.findings.clone(), |c| format!("Ch {}", c), |ch, _| {
            Ok(pipeline::Rescan { found: (ch == 3).then(|| hit(3, 45.0)), silences: vec![] })
        });
        let (mut app, tx) = rescanning_app(ed);
        assert!(draw(&mut app, 80, 24).contains("Re-scanning"));
        app.handle_key(ch('?'));
        assert!(draw(&mut app, 80, 24).contains("Ctrl-c"));
        app.handle_key(ch('x'));
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.confirm_quit && !app.is_over());
        tx.send(jobs::Msg::Done(Ok(result))).unwrap();
        app.tick();
        assert!(!app.confirm_quit && !app.help);
        let Screen::Editor(ed) = &app.screen else { panic!("expected the editor") };
        assert_eq!(ed.plan.findings.breaks.iter().map(|b| b.title.as_str()).collect::<Vec<_>>(), ["One", "Two", "Ch 3", "Three"]);
        assert_eq!(ed.selected, 2);
        assert!(draw(&mut app, 100, 24).contains("Found 1 of 2 missing chapters; 1 still missing."));
    }

    #[test]
    fn ctrl_c_during_a_rescan_asks_first() {
        let (mut app, _tx) = rescanning_app(rescan_editor());
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!app.is_over() && draw(&mut app, 80, 24).contains("Quit"));
        app.handle_key(ch('n'));
        assert!(matches!(app.screen, Screen::Rescanning { .. }));
        app.handle_key(ch('q'));
        app.handle_key(ch('y'));
        assert!(app.is_over());
    }

    #[test]
    fn a_failed_rescan_returns_to_the_editor_with_the_error_and_the_partial_results() {
        let ed = rescan_editor();
        let result = run_rescan(vec![3, 4], ed.plan.findings.clone(), |c| format!("Ch {}", c), |ch, _| {
            if ch == 4 { Err("ffmpeg died".into()) } else { Ok(pipeline::Rescan { found: Some(hit(3, 45.0)), silences: vec![] }) }
        });
        let (mut app, tx) = rescanning_app(ed);
        tx.send(jobs::Msg::Done(Ok(result))).unwrap();
        app.tick();
        let Screen::Editor(ed) = &app.screen else { panic!("expected the editor") };
        assert_eq!(ed.plan.findings.breaks.len(), 4, "the found chapter is kept");
        let screen = draw(&mut app, 120, 24);
        assert!(screen.contains("Stopped early: ffmpeg died"), "{}", screen);
        let (mut app, tx) = rescanning_app(rescan_editor());
        tx.send(jobs::Msg::Done(Err("The background job crashed: x".into()))).unwrap();
        app.tick();
        assert!(in_editor(&app) && draw(&mut app, 100, 24).contains("crashed"));
    }

    #[test]
    fn a_rescan_request_without_an_epub_never_leaves_the_editor() {
        let mut ed = rescan_editor();
        ed.plan.epub = None;
        let mut app = App { screen: Screen::Editor(Box::new(ed)), ..App::new(None).unwrap() };
        app.handle_key(ch('R'));
        assert!(in_editor(&app));
        assert!(draw(&mut app, 100, 24).contains("without an EPUB"));
    }

    #[test]
    fn a_rescan_with_an_unreadable_epub_fails_before_starting_and_says_why() {
        let mut ed = rescan_editor();
        ed.plan.epub = Some("/nonexistent/book.epub".into());
        let mut app = App { screen: Screen::Editor(Box::new(ed)), ..App::new(None).unwrap() };
        app.handle_key(ch('R'));
        assert!(in_editor(&app), "no job was started");
        let Screen::Editor(ed) = &app.screen else { unreachable!() };
        assert!(!ed.status.is_empty());
    }
}
