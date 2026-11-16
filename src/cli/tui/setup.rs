// The Setup screen: where the book is, where the output goes, and the analysis options.
use super::browser::{Browser, BrowserOutcome, Pick};
use super::hints::wrap_hints;
use super::input::Input;
use audiobook_splitter_rust::plan::Settings;
use audiobook_splitter_rust::{collect_inputs, user_path, AUDIO_EXTENSIONS};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use std::path::PathBuf;

const LABELS: [&str; 7] = ["Audio file or folder", "EPUB (optional)", "Output folder", "Minimum silence (s)", "Noise level (dB)", "Match threshold (0-1)", "Use cache"];
const ROWS: usize = LABELS.len();
const CACHE_ROW: usize = 6;

#[derive(Debug)]
pub struct AnalyzeRequest {
    pub inputs: Vec<PathBuf>,
    pub epub: Option<PathBuf>,
    pub out: PathBuf,
    pub settings: Settings,
    pub use_cache: bool,
}

pub enum SetupOutcome {
    Continue,
    Start(AnalyzeRequest),
    Quit,
}

pub struct Setup {
    fields: Vec<Input>, // rows 0..=5
    cache: bool,
    focus: usize,
    browser: Option<(usize, Browser)>,
    feedback: [String; 3], // live notes beside the three path rows
    help: bool,
    pub error: String,
}

// A short note on what a path row's text points at.
fn describe(row: usize, text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return String::new();
    }
    let path = user_path(t);
    match row {
        0 => match collect_inputs(&path) {
            Ok(files) if files.is_empty() => "no audio files".into(),
            Ok(files) => format!("{} audio file{}", files.len(), if files.len() == 1 { "" } else { "s" }),
            Err(_) => "not found".into(),
        },
        1 => if path.is_file() { "found".into() } else { "not found".into() },
        _ => {
            if path.is_dir() {
                "existing folder".into()
            } else if path.exists() {
                "not a folder".into()
            } else {
                "will be created".into()
            }
        }
    }
}

impl Setup {
    pub fn new(prefill: Option<&str>) -> Setup {
        let d = Settings::default();
        let mut s = Setup {
            fields: [prefill.unwrap_or(""), "", "", &d.min_silence.to_string(), &d.noise_db.to_string(), &d.threshold.to_string()].iter().map(|v| Input::new(v)).collect(),
            cache: true,
            focus: 0,
            browser: None,
            feedback: Default::default(),
            help: false,
            error: String::new(),
        };
        s.refresh(0);
        s
    }

    fn refresh(&mut self, row: usize) {
        if row < 3 {
            self.feedback[row] = describe(row, &self.fields[row].value());
        }
    }

    // Checks everything cheap before any slow work starts, like the CLI and the GUI do.
    pub fn request(&self) -> Result<AnalyzeRequest, String> {
        let input = self.fields[0].value();
        if input.trim().is_empty() {
            return Err("Choose an audio file or folder.".into());
        }
        let path = user_path(&input);
        let inputs = collect_inputs(&path)?;
        if inputs.is_empty() {
            return Err(format!("No supported audio files found in '{}'.", path.display()));
        }
        let epub_text = self.fields[1].value();
        let epub = (!epub_text.trim().is_empty()).then(|| user_path(&epub_text));
        if let Some(e) = &epub
            && !e.is_file()
        {
            return Err(format!("The EPUB '{}' was not found.", e.display()));
        }
        let out_text = self.fields[2].value();
        if out_text.trim().is_empty() {
            return Err("Choose an output folder.".into());
        }
        let out = user_path(&out_text);
        if out.exists() && !out.is_dir() {
            return Err(format!("'{}' is not a folder.", out.display()));
        }
        let number = |row: usize, name: &str| self.fields[row].value().trim().parse::<f64>().map_err(|_| format!("{} must be a number.", name));
        let settings = Settings { min_silence: number(3, "Minimum silence")?, noise_db: number(4, "Noise level")?, threshold: number(5, "Match threshold")? };
        settings.validate()?;
        Ok(AnalyzeRequest { inputs, epub, out, settings, use_cache: self.cache })
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SetupOutcome {
        if self.help {
            self.help = false;
            return SetupOutcome::Continue;
        }
        if let Some((row, mut browser)) = self.browser.take() {
            match browser.handle_key(key) {
                BrowserOutcome::Continue => self.browser = Some((row, browser)),
                BrowserOutcome::Cancel => {}
                BrowserOutcome::Chosen(path) => {
                    self.fields[row] = Input::new(&path.display().to_string());
                    self.refresh(row);
                }
            }
            return SetupOutcome::Continue;
        }
        self.error.clear();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return SetupOutcome::Quit,
            KeyCode::F(1) => self.help = true,
            KeyCode::Char('c') if ctrl => return SetupOutcome::Quit,
            KeyCode::Char('a') if ctrl => match self.request() {
                Ok(req) => return SetupOutcome::Start(req),
                Err(e) => self.error = e,
            },
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % ROWS,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + ROWS - 1) % ROWS,
            KeyCode::Enter if self.focus < 3 => {
                let (title, pick) = match self.focus {
                    0 => ("Choose the audio file or folder", Pick::FileOrFolder(&AUDIO_EXTENSIONS)),
                    1 => ("Choose the EPUB", Pick::File(&["epub"])),
                    _ => ("Choose the output folder", Pick::Folder),
                };
                self.browser = Some((self.focus, Browser::new(title, &user_path(&self.fields[self.focus].value()).to_string_lossy(), pick)));
            }
            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right | KeyCode::Enter if self.focus == CACHE_ROW => self.cache = !self.cache,
            _ if self.focus < CACHE_ROW => {
                #[allow(clippy::collapsible_match)] // the guard would hide the side effect of typing
                if self.fields[self.focus].handle_key(key) {
                    self.refresh(self.focus);
                }
            }
            _ => {}
        }
        SetupOutcome::Continue
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" Setup ");
        let inner = block.inner(area);
        f.render_widget(block, area);
        let label_w = 24u16;
        let note_w = 18u16;
        let field_w = inner.width.saturating_sub(label_w + note_w + 2).max(8) as usize;
        let mut lines = Vec::new();
        let mut cursor = None;
        for (row, name) in LABELS.iter().enumerate() {
            let hot = self.focus == row;
            let label_style = if hot { Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED) } else { Style::new().add_modifier(Modifier::BOLD) };
            let label = Span::styled(format!("{:<w$}", name, w = label_w as usize), label_style);
            if row == CACHE_ROW {
                lines.push(Line::from(vec![label, Span::raw(if self.cache { "[x] on" } else { "[ ] off" })]));
            } else {
                let (text, col) = self.fields[row].view(field_w);
                let note = self.feedback.get(row).cloned().unwrap_or_default();
                // A path row with an entry turns green; the live note beside it says whether the path is usable.
                let value_style = if row < 3 && !self.fields[row].value().trim().is_empty() { Style::new().fg(Color::Green) } else { Style::new() };
                lines.push(Line::from(vec![label, Span::styled(format!("{:<w$}", text, w = field_w), value_style), Span::styled(format!("  {}", note), Style::new().fg(Color::Cyan))]));
                if hot {
                    cursor = Some((inner.x + label_w + col as u16, inner.y + row as u16));
                }
            }
        }
        lines.push(Line::raw(""));
        for row in super::hints::wrap_text(&self.error, inner.width as usize) {
            lines.push(Line::styled(row, Style::new().fg(Color::Yellow)));
        }
        lines.extend(wrap_hints(&[("Tab", "next"), ("Enter", "browse (path rows)"), ("Space", "toggle"), ("Ctrl-A", "analyze"), ("F1", "help"), ("Esc", "quit")], "   ", inner.width as usize));
        f.render_widget(Paragraph::new(lines), inner);
        match &mut self.browser {
            Some((_, browser)) => browser.render(f, area),
            None => {
                if let Some(pos) = cursor {
                    f.set_cursor_position(pos);
                }
            }
        }
        if self.help {
            super::help::render_help(f, area, "Setup keys", super::help::SETUP_KEYS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    // A folder holding one (empty) mp3, and a path for the output beside it.
    fn folder(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("abs-setup-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("book.mp3"), "x").unwrap();
        (dir.clone(), dir.join("out"))
    }

    // A Setup whose input and output are valid and whose options are the defaults.
    fn filled(name: &str) -> Setup {
        let (dir, out) = folder(name);
        let mut s = Setup::new(Some(dir.to_str().unwrap()));
        s.fields[2] = Input::new(out.to_str().unwrap());
        s
    }

    fn screen_of(s: &mut Setup, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| s.render(f, f.area())).unwrap();
        let buf = t.backend().buffer();
        (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn a_long_warning_wraps_instead_of_being_cut_off() {
        let mut s = Setup::new(None);
        s.error = "Whisper model 'ggml-base.en.bin' not found. Download it in the Models folder.".into();
        let screen = screen_of(&mut s, 50, 24);
        assert!(screen.contains("Models folder."), "{}", screen);
    }

    #[test]
    fn f1_opens_help_and_any_key_closes_it_without_typing() {
        let mut s = Setup::new(None);
        s.handle_key(k(KeyCode::F(1)));
        assert!(s.help);
        s.handle_key(k(KeyCode::Char('x')));
        assert!(!s.help);
        assert_eq!(s.fields[0].value(), "", "the key that closed the help is not typed");
        s.handle_key(k(KeyCode::F(1)));
        let screen = screen_of(&mut s, 80, 24);
        assert!(screen.contains("Ctrl-U") && screen.contains("clear the field"), "only the help popup lists these:\n{}", screen);
    }

    #[test]
    fn the_hint_keeps_ctrl_a_and_f1_visible_at_60x15() {
        let mut s = Setup::new(None);
        let screen = screen_of(&mut s, 60, 15);
        assert!(screen.contains("Ctrl-A") && screen.contains("F1") && screen.contains("Esc quit"), "{}", screen);
    }

    #[test]
    fn the_browser_starts_at_the_cleaned_up_path() {
        let (dir, _) = folder("quoted");
        let mut s = Setup::new(Some(&format!("  \"{}\"  ", dir.display())));
        s.handle_key(k(KeyCode::Enter));
        let (_, b) = s.browser.as_ref().unwrap();
        assert_eq!(b.dir(), dir.as_path());
    }

    #[test]
    fn the_defaults_match_the_analyze_flags() {
        let (dir, out) = folder("defaults");
        let mut s = Setup::new(Some(dir.to_str().unwrap()));
        s.fields[2] = Input::new(out.to_str().unwrap());
        let req = s.request().unwrap();
        assert_eq!(req.settings, Settings::default());
        assert_eq!((req.inputs.len(), req.epub, req.use_cache), (1, None, true));
        assert_eq!(req.out, out);
    }

    #[test]
    fn it_explains_what_is_missing_or_wrong() {
        assert!(Setup::new(None).request().unwrap_err().contains("audio file or folder"));
        let (dir, out) = folder("errors");
        let s = Setup::new(Some(dir.to_str().unwrap()));
        assert!(s.request().unwrap_err().contains("output folder"));
        for (row, bad, want) in [(3, "0", "Minimum silence"), (3, "abc", "must be a number"), (4, "x", "Noise level"), (5, "2", "between 0 and 1")] {
            let mut s = filled(&format!("bad{}{}", row, bad));
            s.fields[row] = Input::new(bad);
            assert!(s.request().unwrap_err().contains(want), "{:?} in row {} should mention {:?}", bad, row, want);
        }
        let mut s = filled("missing-input");
        s.fields[0] = Input::new(dir.join("nothing").to_str().unwrap());
        assert!(s.request().is_err(), "a missing input is refused");
        let mut s = filled("epub");
        s.fields[1] = Input::new("/definitely/missing.epub");
        assert!(s.request().unwrap_err().contains("EPUB"));
        let mut s = filled("out-is-file");
        std::fs::write(&out, "i am a file").unwrap();
        s.fields[2] = Input::new(out.to_str().unwrap());
        assert!(s.request().unwrap_err().contains("is not a folder"));
    }

    #[test]
    fn tab_moves_through_the_rows_and_wraps() {
        let mut s = Setup::new(None);
        for _ in 0..7 {
            s.handle_key(k(KeyCode::Tab));
        }
        assert_eq!(s.focus, 0);
        s.handle_key(k(KeyCode::BackTab));
        assert_eq!(s.focus, 6);
    }

    #[test]
    fn the_cache_toggle_flips_with_space() {
        let mut s = filled("cache");
        s.focus = 6;
        s.handle_key(k(KeyCode::Char(' ')));
        assert!(!s.request().unwrap().use_cache);
        s.handle_key(k(KeyCode::Right));
        assert!(s.request().unwrap().use_cache);
    }

    #[test]
    fn enter_on_a_path_opens_the_browser_and_a_choice_fills_the_field() {
        let (dir, _) = folder("browse");
        let mut s = Setup::new(None);
        s.handle_key(k(KeyCode::Enter));
        assert!(s.browser.is_some());
        s.handle_key(k(KeyCode::Esc));
        assert!(s.browser.is_none(), "Esc closes the browser, not the app");
        s.browser = Some((0, Browser::new("Choose", dir.to_str().unwrap(), Pick::FileOrFolder(&["mp3"]))));
        s.handle_key(k(KeyCode::Char(' '))); // choose the folder itself
        assert!(s.browser.is_none());
        assert_eq!(s.fields[0].value(), dir.display().to_string());
        assert_eq!(s.feedback[0], "1 audio file");
    }

    #[test]
    fn ctrl_a_starts_and_esc_or_ctrl_c_quit() {
        let mut s = filled("start");
        assert!(matches!(s.handle_key(ctrl('a')), SetupOutcome::Start(_)));
        let mut bad = Setup::new(None);
        assert!(matches!(bad.handle_key(ctrl('a')), SetupOutcome::Continue));
        assert!(bad.error.contains("audio file or folder"));
        assert!(matches!(bad.handle_key(k(KeyCode::Esc)), SetupOutcome::Quit));
        assert!(matches!(bad.handle_key(ctrl('c')), SetupOutcome::Quit));
    }

    #[test]
    fn a_path_row_with_an_entry_is_drawn_green() {
        let mut s = filled("green"); // input and output set, EPUB left empty
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        t.draw(|f| s.render(f, f.area())).unwrap();
        let buf = t.backend().buffer();
        let (x, y0) = (25, 1); // the value column (border + 24-wide label), first row inside the border
        assert_eq!(buf[(x, y0)].fg, Color::Green, "the audio path has an entry");
        assert_eq!(buf[(x, y0 + 2)].fg, Color::Green, "the output folder has an entry");
        assert_ne!(buf[(x, y0 + 1)].fg, Color::Green, "the empty EPUB row stays plain");
        assert_ne!(buf[(x, y0 + 3)].fg, Color::Green, "number rows are never green");
    }

    #[test]
    fn it_draws_the_labels_feedback_and_hints_at_80x24_and_60x15() {
        let mut s = filled("draw");
        for (w, h) in [(80, 24), (60, 15), (30, 8)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| s.render(f, f.area())).unwrap();
            if w == 80 {
                let buf = t.backend().buffer();
                let screen: String = (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n");
                for want in ["Audio file or folder", "EPUB (optional)", "Output folder", "Minimum silence", "Noise level", "Match threshold", "Use cache", "1 audio file", "Ctrl-A"] {
                    assert!(screen.contains(want), "missing {:?} in\n{}", want, screen);
                }
            }
        }
    }

    #[test]
    fn the_hints_wrap_between_commands_at_60_columns() {
        let mut s = filled("hintwrap");
        let mut t = Terminal::new(TestBackend::new(60, 24)).unwrap();
        t.draw(|f| s.render(f, f.area())).unwrap();
        let buf = t.backend().buffer();
        let rows: Vec<String> = (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect();
        for want in ["Tab next", "Enter browse (path rows)", "Space toggle", "Ctrl-A analyze", "F1 help", "Esc quit"] {
            assert!(rows.iter().any(|l| l.contains(want)), "{:?} not whole on one line:\n{}", want, rows.join("\n"));
        }
    }
}
