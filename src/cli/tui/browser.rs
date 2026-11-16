// A file browser popup: pick a file or folder with the keyboard, or type a path.
use super::hints::wrap_hints;
use super::input::Input;
use audiobook_splitter_rust::user_path;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
pub enum Pick {
    Folder,
    File(&'static [&'static str]),
    FileOrFolder(&'static [&'static str]),
}

impl Pick {
    fn extensions(self) -> &'static [&'static str] {
        match self {
            Pick::Folder => &[],
            Pick::File(x) | Pick::FileOrFolder(x) => x,
        }
    }

    fn folders_pickable(self) -> bool {
        !matches!(self, Pick::File(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

pub enum BrowserOutcome {
    Continue,
    Chosen(PathBuf),
    Cancel,
}

fn wanted(name: &str, exts: &[&str]) -> bool {
    Path::new(name).extension().and_then(|e| e.to_str()).is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

// What `pick` can show in `dir`: folders first, then matching files, each sorted without regard to case.
pub fn list_dir(dir: &Path, pick: Pick, hidden: bool) -> Result<Vec<Entry>, String> {
    let read = std::fs::read_dir(dir).map_err(|e| format!("Can't read {}: {}", dir.display(), e))?;
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                return None;
            }
            let is_dir = e.path().is_dir(); // follows symlinks
            let show = match pick {
                Pick::Folder => is_dir,
                Pick::File(x) | Pick::FileOrFolder(x) => is_dir || wanted(&name, x),
            };
            show.then_some(Entry { name, is_dir })
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

fn home_or_cwd() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub struct Browser {
    title: String,
    dir: PathBuf,
    pick: Pick,
    hidden: bool,
    all: Vec<Entry>,
    filter: String,
    filtering: bool,
    typed: Option<Input>,
    naming: Option<Input>,
    selected: usize,
    state: ListState,
    pub(super) error: String,
}

impl Browser {
    // Opens at `start` if that is a folder, at the folder holding it (with it selected) if it
    // is a file, else at the home folder.
    pub fn new(title: &str, start: &str, pick: Pick) -> Browser {
        let given = user_path(start);
        let (dir, name) = if start.trim().is_empty() {
            (home_or_cwd(), None)
        } else if given.is_dir() {
            (given, None)
        } else if let Some(parent) = given.parent().filter(|p| p.is_dir()) {
            (parent.to_path_buf(), given.file_name().map(|n| n.to_string_lossy().into_owned()))
        } else {
            (home_or_cwd(), None)
        };
        let mut b = Browser {
            title: title.to_string(),
            dir,
            pick,
            hidden: false,
            all: Vec::new(),
            filter: String::new(),
            filtering: false,
            typed: None,
            naming: None,
            selected: 0,
            state: ListState::default(),
            error: String::new(),
        };
        b.reload(name.as_deref());
        b
    }

    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn selected_name(&self) -> Option<String> {
        self.visible().get(self.selected).map(|e| e.name.clone())
    }

    fn visible(&self) -> Vec<&Entry> {
        let needle = self.filter.to_lowercase();
        self.all.iter().filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle)).collect()
    }

    // Re-reads the folder, clears the filter and selects `name` if it is listed.
    fn reload(&mut self, name: Option<&str>) {
        match list_dir(&self.dir, self.pick, self.hidden) {
            Ok(all) => {
                self.all = all;
                self.error.clear();
            }
            Err(e) => {
                self.all.clear();
                self.error = e;
            }
        }
        self.filter.clear();
        self.filtering = false;
        self.selected = name.and_then(|n| self.all.iter().position(|e| e.name == n)).unwrap_or(0);
    }

    fn enter(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.reload(None);
    }

    fn up(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            let left = self.dir.file_name().map(|n| n.to_string_lossy().into_owned());
            self.dir = parent;
            self.reload(left.as_deref());
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> BrowserOutcome {
        if self.typed.is_some() {
            return self.key_typed(key);
        }
        if self.naming.is_some() {
            self.key_naming(key);
            return BrowserOutcome::Continue;
        }
        self.error.clear();
        if self.filtering {
            self.key_filter(key);
            return BrowserOutcome::Continue;
        }
        let last = self.visible().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    return BrowserOutcome::Cancel;
                }
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(last),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            KeyCode::Enter => {
                if let Some(e) = self.visible().get(self.selected).map(|e| (*e).clone()) {
                    let path = self.dir.join(&e.name);
                    if e.is_dir {
                        self.enter(path);
                    } else {
                        return BrowserOutcome::Chosen(path);
                    }
                }
            }
            KeyCode::Right => {
                if let Some(e) = self.visible().get(self.selected).filter(|e| e.is_dir).map(|e| (*e).clone()) {
                    self.enter(self.dir.join(&e.name));
                }
            }
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => self.up(),
            KeyCode::Char('~') => self.enter(home_or_cwd()),
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Char('.') => {
                let keep = self.selected_name();
                self.hidden = !self.hidden;
                self.reload(keep.as_deref());
            }
            KeyCode::Tab => {
                let start = format!("{}{}", self.dir.display(), std::path::MAIN_SEPARATOR);
                self.typed = Some(Input::new(&start));
                self.error.clear();
            }
            KeyCode::Char('n') if matches!(self.pick, Pick::Folder) => self.naming = Some(Input::new("")),
            KeyCode::Char(' ') if self.pick.folders_pickable() => return BrowserOutcome::Chosen(self.dir.clone()),
            _ => {}
        }
        BrowserOutcome::Continue
    }

    fn key_filter(&mut self, key: KeyEvent) {
        let before = self.filter.clone();
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.filtering = false;
            }
            KeyCode::Enter => self.filtering = false,
            KeyCode::Backspace => {
                if self.filter.pop().is_none() {
                    self.filtering = false;
                }
            }
            KeyCode::Char(c) => self.filter.push(c),
            _ => {}
        }
        if self.filter != before {
            self.selected = 0;
        }
    }

    // Names a new folder inside the current one; Enter creates it and selects it.
    fn key_naming(&mut self, key: KeyEvent) {
        let Some(input) = self.naming.as_mut() else { return };
        match key.code {
            KeyCode::Esc => self.naming = None,
            KeyCode::Enter => {
                let name = input.value().trim().to_string();
                if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
                    self.error = "A folder name can't be empty or contain a path separator.".into();
                    return;
                }
                match std::fs::create_dir(self.dir.join(&name)) {
                    Ok(()) => {
                        self.naming = None;
                        self.reload(Some(&name));
                    }
                    Err(e) => self.error = format!("Can't create '{}': {}", name, e),
                }
            }
            _ => {
                self.error.clear();
                input.handle_key(key);
            }
        }
    }

    fn key_typed(&mut self, key: KeyEvent) -> BrowserOutcome {
        let Some(input) = self.typed.as_mut() else { return BrowserOutcome::Continue };
        match key.code {
            KeyCode::Esc => self.typed = None,
            KeyCode::Enter => {
                let mut path = user_path(&input.value());
                if path.is_relative() {
                    path = self.dir.join(path);
                }
                if path.is_dir() {
                    self.typed = None;
                    if self.pick.folders_pickable() {
                        return BrowserOutcome::Chosen(path);
                    }
                    self.enter(path);
                } else if matches!(self.pick, Pick::Folder) && !path.exists() && path.parent().is_some_and(|p| p.is_dir()) {
                    self.typed = None;
                    return BrowserOutcome::Chosen(path);
                } else if path.is_file() && !matches!(self.pick, Pick::Folder) && wanted(&path.to_string_lossy(), self.pick.extensions()) {
                    return BrowserOutcome::Chosen(path);
                } else {
                    self.error = format!("'{}' was not found (or is not a type you can pick here).", path.display());
                }
            }
            _ => {
                input.handle_key(key);
            }
        }
        BrowserOutcome::Continue
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let rect = super::editor::centered(area, (area.width * 9 / 10).max(30), area.height.saturating_sub(2).max(5));
        f.render_widget(Clear, rect);
        let block = Block::bordered().title(format!(" {} ", self.title));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let pairs: &[(&str, &str)] = match self.pick {
            Pick::Folder => &[("Enter", "open"), ("Space", "choose this folder"), ("n", "new folder"), ("Backspace", "up"), ("/", "filter"), (".", "hidden"), ("Tab", "type path"), ("Esc", "cancel")],
            Pick::File(_) => &[("Enter", "open or pick"), ("Backspace", "up"), ("/", "filter"), (".", "hidden"), ("Tab", "type path"), ("Esc", "cancel")],
            Pick::FileOrFolder(_) => &[("Enter", "open or pick"), ("Space", "choose this folder"), ("Backspace", "up"), ("/", "filter"), (".", "hidden"), ("Tab", "type path"), ("Esc", "cancel")],
        };
        // The hints wrap between commands (at most 3 lines); the error replaces them on one line.
        let mut hint_lines = wrap_hints(pairs, "  ", inner.width as usize);
        hint_lines.truncate(3);
        let hint_h = if self.error.is_empty() { (hint_lines.len() as u16).min(inner.height.saturating_sub(2)).max(1) } else { 1 };
        let [path_row, list_row, hint_row] = Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(hint_h)]).areas(inner);

        let mut cursor = None;
        let path_line = match &self.typed {
            Some(input) => {
                let (text, col) = input.view(path_row.width.saturating_sub(7) as usize);
                cursor = Some((path_row.x + 6 + col as u16, path_row.y));
                format!("Path: {}", text)
            }
            None if self.naming.is_some() => {
                let input = self.naming.as_ref().unwrap();
                let (text, col) = input.view(path_row.width.saturating_sub(13) as usize);
                cursor = Some((path_row.x + 12 + col as u16, path_row.y));
                format!("New folder: {}", text)
            }
            None if self.filtering || !self.filter.is_empty() => format!("Filter: {}  ({})", self.filter, self.dir.display()),
            None => self.dir.display().to_string(),
        };
        f.render_widget(Paragraph::new(path_line), path_row);

        let items: Vec<ListItem> = self
            .visible()
            .iter()
            .map(|e| if e.is_dir { ListItem::new(format!("{}/", e.name)).style(Style::new().add_modifier(Modifier::BOLD)) } else { ListItem::new(e.name.clone()) })
            .collect();
        if items.is_empty() {
            f.render_widget(Paragraph::new("(nothing here)"), list_row);
        } else {
            self.state.select(Some(self.selected));
            let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED)).highlight_symbol(">");
            f.render_stateful_widget(list, list_row, &mut self.state);
        }

        let line = if self.error.is_empty() { Paragraph::new(hint_lines) } else { Paragraph::new(self.error.clone()).style(Style::new().fg(Color::Yellow)) };
        f.render_widget(line, hint_row);
        if let Some(pos) = cursor {
            f.set_cursor_position(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyModifiers;
    use ratatui::Terminal;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        k(KeyCode::Char(c))
    }

    // tree/: A/, b/ (inner.mp3), x.mp3, y.txt, .hidden.mp3, cover.PNG
    fn tree(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("abs-browser-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("A")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        for f in ["b/inner.mp3", "x.mp3", "y.txt", ".hidden.mp3", "cover.PNG"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        dir
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(|e| if e.is_dir { format!("{}/", e.name) } else { e.name.clone() }).collect()
    }

    const MP3: Pick = Pick::File(&["mp3"]);

    #[test]
    fn listing_puts_folders_first_and_filters_by_extension_and_hidden() {
        let dir = tree("list");
        assert_eq!(names(&list_dir(&dir, MP3, false).unwrap()), ["A/", "b/", "x.mp3"]);
        assert_eq!(names(&list_dir(&dir, MP3, true).unwrap()), ["A/", "b/", ".hidden.mp3", "x.mp3"]);
        assert_eq!(names(&list_dir(&dir, Pick::Folder, false).unwrap()), ["A/", "b/"]);
        assert_eq!(names(&list_dir(&dir, Pick::File(&["png"]), false).unwrap()), ["A/", "b/", "cover.PNG"], "extensions match in any case");
        assert!(list_dir(&dir.join("nope"), MP3, false).unwrap_err().contains("Can't read"));
    }

    #[test]
    fn right_arrow_opens_a_folder_but_never_picks_a_file() {
        let dir = tree("right");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        b.handle_key(ch('j'));
        b.handle_key(k(KeyCode::Right));
        assert_eq!(b.dir(), dir.join("b"));
        assert_eq!(b.selected_name().as_deref(), Some("inner.mp3"));
        assert!(matches!(b.handle_key(k(KeyCode::Right)), BrowserOutcome::Continue), "a file is not picked by Right");
    }

    #[test]
    fn enter_opens_folders_picks_files_and_backspace_goes_up() {
        let dir = tree("walk");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        assert_eq!(b.selected_name().as_deref(), Some("A"));
        b.handle_key(ch('j'));
        assert!(matches!(b.handle_key(k(KeyCode::Enter)), BrowserOutcome::Continue));
        assert_eq!(b.dir(), dir.join("b"));
        assert_eq!(b.selected_name().as_deref(), Some("inner.mp3"));
        b.handle_key(k(KeyCode::Backspace));
        assert_eq!(b.dir(), dir);
        assert_eq!(b.selected_name().as_deref(), Some("b"), "the folder just left is selected again");
        b.handle_key(k(KeyCode::Enter));
        let BrowserOutcome::Chosen(p) = b.handle_key(k(KeyCode::Enter)) else { panic!("expected a choice") };
        assert_eq!(p, dir.join("b").join("inner.mp3"));
    }

    #[test]
    fn space_chooses_the_folder_only_when_folders_are_pickable() {
        let dir = tree("space");
        let mut files = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        assert!(matches!(files.handle_key(ch(' ')), BrowserOutcome::Continue));
        let mut folders = Browser::new("Pick", dir.to_str().unwrap(), Pick::Folder);
        assert!(matches!(folders.handle_key(ch(' ')), BrowserOutcome::Chosen(p) if p == dir));
        let mut either = Browser::new("Pick", dir.to_str().unwrap(), Pick::FileOrFolder(&["mp3"]));
        assert!(matches!(either.handle_key(ch(' ')), BrowserOutcome::Chosen(p) if p == dir));
        let mut either = Browser::new("Pick", dir.to_str().unwrap(), Pick::FileOrFolder(&["mp3"]));
        either.handle_key(k(KeyCode::End));
        assert!(matches!(either.handle_key(k(KeyCode::Enter)), BrowserOutcome::Chosen(p) if p == dir.join("x.mp3")));
    }

    #[test]
    fn slash_filters_the_list_and_esc_clears_it() {
        let dir = tree("filter");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        b.handle_key(ch('/'));
        b.handle_key(ch('X'));
        assert_eq!(b.selected_name().as_deref(), Some("x.mp3"), "case-insensitive substring match");
        b.handle_key(k(KeyCode::Enter)); // keep the filter, back to normal keys
        let BrowserOutcome::Chosen(p) = b.handle_key(k(KeyCode::Enter)) else { panic!("expected a choice") };
        assert_eq!(p, dir.join("x.mp3"));
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        b.handle_key(ch('/'));
        b.handle_key(ch('z'));
        assert_eq!(b.selected_name(), None);
        b.handle_key(k(KeyCode::Esc));
        assert_eq!(b.selected_name().as_deref(), Some("A"), "Esc clears the filter");
        assert!(matches!(b.handle_key(k(KeyCode::Esc)), BrowserOutcome::Cancel));
    }

    #[test]
    fn n_creates_a_folder_in_the_folder_picker_and_selects_it() {
        let dir = tree("newdir");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), Pick::Folder);
        b.handle_key(ch('n'));
        for c in "Fresh".chars() {
            b.handle_key(ch(c));
        }
        assert!(matches!(b.handle_key(k(KeyCode::Enter)), BrowserOutcome::Continue));
        assert!(dir.join("Fresh").is_dir());
        assert_eq!(b.selected_name().as_deref(), Some("Fresh"));
        // a duplicate or a path-like name is refused and the prompt stays open
        for bad in ["Fresh", "a/b", ""] {
            b.handle_key(ch('n'));
            for c in bad.chars() {
                b.handle_key(ch(c));
            }
            b.handle_key(k(KeyCode::Enter));
            assert!(!b.error.is_empty(), "{:?}", bad);
            b.handle_key(k(KeyCode::Esc));
        }
        assert!(matches!(b.handle_key(k(KeyCode::Esc)), BrowserOutcome::Cancel), "Esc closed the prompt first");
        let mut files = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        files.handle_key(ch('n'));
        files.handle_key(ch('x'));
        files.handle_key(k(KeyCode::Enter));
        assert!(!dir.join("x").exists(), "n does nothing in a file picker");
    }

    #[test]
    fn dot_toggles_hidden_entries() {
        let dir = tree("hidden");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        b.handle_key(k(KeyCode::End));
        assert_eq!(b.selected_name().as_deref(), Some("x.mp3"));
        b.handle_key(ch('.'));
        b.handle_key(ch('/'));
        b.handle_key(ch('h'));
        assert_eq!(b.selected_name().as_deref(), Some(".hidden.mp3"));
    }

    #[test]
    fn tab_lets_you_type_a_path() {
        let dir = tree("typed");
        let mut files = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        files.handle_key(k(KeyCode::Tab));
        files.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        for c in dir.join("b").to_str().unwrap().chars() {
            files.handle_key(ch(c));
        }
        assert!(matches!(files.handle_key(k(KeyCode::Enter)), BrowserOutcome::Continue));
        assert_eq!(files.dir(), dir.join("b"), "a typed folder is entered when only files can be picked");
        let mut folders = Browser::new("Pick", dir.to_str().unwrap(), Pick::Folder);
        folders.handle_key(k(KeyCode::Tab));
        folders.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        for c in dir.join("b").to_str().unwrap().chars() {
            folders.handle_key(ch(c));
        }
        assert!(matches!(folders.handle_key(k(KeyCode::Enter)), BrowserOutcome::Chosen(p) if p == dir.join("b")));
        let mut bad = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        bad.handle_key(k(KeyCode::Tab));
        for c in "/nope".chars() {
            bad.handle_key(ch(c));
        }
        assert!(matches!(bad.handle_key(k(KeyCode::Enter)), BrowserOutcome::Continue));
        assert!(bad.error.contains("not found"));
        assert!(matches!(bad.handle_key(k(KeyCode::Esc)), BrowserOutcome::Continue), "Esc leaves typing mode first");
        assert!(matches!(bad.handle_key(k(KeyCode::Esc)), BrowserOutcome::Cancel));
    }

    fn type_path(b: &mut Browser, path: &Path) {
        b.handle_key(k(KeyCode::Tab));
        b.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        for c in path.to_str().unwrap().chars() {
            b.handle_key(ch(c));
        }
    }

    #[test]
    fn a_folder_only_browser_refuses_a_typed_file_path() {
        let dir = tree("typedfolderonly");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), Pick::Folder);
        type_path(&mut b, &dir.join("x.mp3"));
        assert!(matches!(b.handle_key(k(KeyCode::Enter)), BrowserOutcome::Continue));
        assert!(b.error.contains("not found"));
    }

    #[test]
    fn a_folder_browser_accepts_a_new_folder_whose_parent_exists() {
        let dir = tree("typednew");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), Pick::Folder);
        type_path(&mut b, &dir.join("fresh"));
        assert!(matches!(b.handle_key(k(KeyCode::Enter)), BrowserOutcome::Chosen(p) if p == dir.join("fresh")));
        assert!(!dir.join("fresh").exists(), "the browser only names it; the split creates it");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), Pick::Folder);
        type_path(&mut b, &dir.join("no").join("fresh"));
        assert!(matches!(b.handle_key(k(KeyCode::Enter)), BrowserOutcome::Continue));
        assert!(b.error.contains("not found"));
        let mut files = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        type_path(&mut files, &dir.join("fresh"));
        assert!(matches!(files.handle_key(k(KeyCode::Enter)), BrowserOutcome::Continue), "only the folder picker may name a new path");
    }

    #[test]
    fn a_file_browser_accepts_a_typed_file_only_when_the_extension_matches() {
        let dir = tree("typedfile");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        type_path(&mut b, &dir.join("x.mp3"));
        assert!(matches!(b.handle_key(k(KeyCode::Enter)), BrowserOutcome::Chosen(p) if p == dir.join("x.mp3")));
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        type_path(&mut b, &dir.join("y.txt"));
        assert!(matches!(b.handle_key(k(KeyCode::Enter)), BrowserOutcome::Continue));
        assert!(b.error.contains("not found"));
    }

    #[test]
    fn navigation_keys_in_the_filter_box_do_not_reset_the_selection() {
        let dir = tree("filternav");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        b.handle_key(k(KeyCode::End));
        b.handle_key(ch('/'));
        for code in [KeyCode::Down, KeyCode::Up, KeyCode::PageDown, KeyCode::PageUp] {
            b.handle_key(k(code));
            assert_eq!(b.selected_name().as_deref(), Some("x.mp3"));
        }
    }

    #[test]
    fn a_typed_path_error_clears_on_the_next_key_after_leaving_typing() {
        let dir = tree("errclear");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        type_path(&mut b, &dir.join("nope"));
        b.handle_key(k(KeyCode::Enter));
        assert!(!b.error.is_empty());
        b.handle_key(k(KeyCode::Esc));
        b.handle_key(ch('j'));
        assert!(b.error.is_empty());
    }

    #[test]
    fn a_start_path_that_is_a_file_opens_its_folder_with_the_file_selected() {
        let dir = tree("startfile");
        let b = Browser::new("Pick", dir.join("x.mp3").to_str().unwrap(), MP3);
        assert_eq!((b.dir(), b.selected_name().as_deref()), (dir.as_path(), Some("x.mp3")));
        let empty = Browser::new("Pick", "", MP3);
        assert!(empty.dir().is_dir(), "an empty start opens the home or current folder");
    }

    #[test]
    fn it_draws_the_title_the_path_and_the_entries() {
        let dir = tree("draw");
        let mut b = Browser::new("Choose the book", dir.to_str().unwrap(), MP3);
        for (w, h) in [(80, 24), (60, 15), (30, 6)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| b.render(f, f.area())).unwrap();
            if w == 80 {
                let buf = t.backend().buffer();
                let screen: String = (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n");
                for want in ["Choose the book", "A/", "b/", "x.mp3"] {
                    assert!(screen.contains(want), "missing {:?} in\n{}", want, screen);
                }
                assert!(!screen.contains("y.txt"), "files that don't match the filter are not listed");
            }
        }
    }

    #[test]
    fn the_hint_keys_are_bold_cyan_and_the_descriptions_are_plain() {
        let dir = tree("hintstyle");
        let mut b = Browser::new("Pick", dir.to_str().unwrap(), MP3);
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        t.draw(|f| b.render(f, f.area())).unwrap();
        let buf = t.backend().buffer();
        let (x, y) = super::super::hints::find_text(buf, "Enter open").expect("the hint row");
        let key = buf[(x, y)].style();
        assert_eq!(key.fg, Some(Color::Cyan));
        assert!(key.add_modifier.contains(Modifier::BOLD));
        let desc = buf[(x + 6, y)].style();
        assert_ne!(desc.fg, Some(Color::Cyan));
        assert!(!desc.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_hint_wraps_between_commands_at_60_columns() {
        let dir = tree("hintwrap");
        for pick in [Pick::FileOrFolder(&["mp3"]), Pick::Folder] {
            let folder = matches!(pick, Pick::Folder);
            let mut b = Browser::new("Pick", dir.to_str().unwrap(), MP3);
            b.pick = pick;
            let mut t = Terminal::new(TestBackend::new(60, 24)).unwrap();
            t.draw(|f| b.render(f, f.area())).unwrap();
            let buf = t.backend().buffer();
            let rows: Vec<String> = (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect();
            let open = if folder { "Enter open" } else { "Enter open or pick" };
            for want in [open, "Space choose this folder", "Backspace up", "/ filter", ". hidden", "Tab type path", "Esc cancel"] {
                assert!(rows.iter().any(|l| l.contains(want)), "{:?} not whole on one line:\n{}", want, rows.join("\n"));
            }
        }
    }
}
