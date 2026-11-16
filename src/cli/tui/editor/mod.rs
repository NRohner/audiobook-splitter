// The full-screen chapter editor: a table of chapters, what Whisper heard around the selected
// break, and single-key actions. `handle_key` and `render` (view.rs) do no terminal I/O, so
// tests drive them with synthetic keys and ratatui's TestBackend. Every change goes through
// `apply`, which snapshots first so undo/redo and refusals are uniform.
mod view;
pub(crate) use view::centered;

use super::input::Input;
use super::tags::{self, FormOutcome, TagsForm};
use crate::cli::edit::ops::{self, Pos};
use crate::cli::preview::write_clip;
use crate::cli::time::{clock, parse_time};
use crate::cli::place;
use audiobook_splitter_rust::meta::Metadata;
use audiobook_splitter_rust::pipeline::file_name;
use audiobook_splitter_rust::plan::{Plan, PlanBreak};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

const PAGE: usize = 10;

pub enum Outcome {
    Continue,
    Save,
    Discard,
    Back, // Only when `allow_back` is set: return to the Setup screen.
}

#[derive(Clone)]
struct Snapshot {
    breaks: Vec<PlanBreak>,
    metadata: Metadata,
    selected: usize,
}

pub(super) enum Prompt {
    Rename(usize),
    AddTime(usize),      // file index
    AddTitle(usize, f64), // file index, seconds
    Jump,
}

pub(super) enum ListKind {
    AddFile,
    Silence { spots: Vec<Pos>, title: Option<String> }, // `title` presets the title prompt
    Missing,
}

pub(super) enum Mode {
    Normal,
    Prompt { kind: Prompt, input: Input },
    List { kind: ListKind, items: Vec<String>, selected: usize },
    Tags(Box<TagsForm>),
    ConfirmQuit,
    ConfirmBack,
    Help,
}

pub struct Editor {
    pub plan: Plan,
    pub selected: usize,
    pub allow_back: bool,
    original: (Vec<PlanBreak>, Metadata),
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    pub(super) table: TableState,
    pub(super) mode: Mode,
    pub(super) status: String,
    // Which file carries the cover picture, found on the first `t` (the audio doesn't change while editing).
    embedded_cover: Option<Option<String>>,
}

impl Editor {
    // Shows `text` in the footer (errors from outside the editor, such as a refused split).
    pub fn set_status(&mut self, text: String) {
        self.status = text;
    }

    pub fn new(plan: Plan) -> Editor {
        let original = (plan.findings.breaks.clone(), plan.metadata.clone());
        Editor {
            plan,
            selected: 0,
            allow_back: false,
            original,
            undo: Vec::new(),
            redo: Vec::new(),
            table: TableState::default(),
            mode: Mode::Normal,
            status: String::new(),
            embedded_cover: None,
        }
    }

    // What `s` does here: the tui app splits at once, edit/run save and carry on.
    fn finish_verb(&self) -> &'static str {
        if self.allow_back { "split" } else { "save" }
    }

    // Opens the tags form. `lookup` (ffprobe over the files) runs only the first time.
    fn open_tags(&mut self, lookup: impl FnOnce(&Plan) -> Option<String>) {
        let plan = &self.plan;
        let embedded = self.embedded_cover.get_or_insert_with(|| lookup(plan)).clone();
        self.mode = Mode::Tags(Box::new(TagsForm::from_plan_with(&self.plan, embedded)));
    }

    // Whether the chapters or tags differ from what the editor started with.
    pub fn changed(&self) -> bool {
        ops::changed(&self.plan, &self.original)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { breaks: self.plan.findings.breaks.clone(), metadata: self.plan.metadata.clone(), selected: self.selected }
    }

    fn restore(&mut self, s: Snapshot) {
        self.plan.findings.breaks = s.breaks;
        self.plan.metadata = s.metadata;
        self.selected = s.selected;
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.plan.findings.breaks.len().saturating_sub(1));
    }

    // Runs one change; on success it becomes undoable, on refusal the plan is put back as it was.
    fn apply(&mut self, change: impl FnOnce(&mut Plan) -> Result<(), String>) -> Result<(), String> {
        let before = self.snapshot();
        match change(&mut self.plan) {
            Ok(()) => {
                self.undo.push(before);
                self.redo.clear();
                self.clamp();
                Ok(())
            }
            Err(e) => {
                self.restore(before);
                Err(e)
            }
        }
    }

    // `apply` for keys that have no prompt to stay in: a refusal goes to the status line.
    fn edit(&mut self, change: impl FnOnce(&mut Plan) -> Result<(), String>) {
        if let Err(e) = self.apply(change) {
            self.status = e;
        }
    }

    fn undo(&mut self) {
        match self.undo.pop() {
            Some(prev) => {
                let now = self.snapshot();
                self.redo.push(now);
                self.restore(prev);
                self.status = "Undone.".into();
            }
            None => self.status = "Nothing to undo.".into(),
        }
    }

    fn redo(&mut self) {
        match self.redo.pop() {
            Some(next) => {
                let now = self.snapshot();
                self.undo.push(now);
                self.restore(next);
                self.status = "Redone.".into();
            }
            None => self.status = "Nothing to redo.".into(),
        }
    }

    fn select_pos(&mut self, at: Pos) {
        if let Some(i) = self.plan.findings.breaks.iter().position(|b| ops::same((b.file, b.at), at)) {
            self.selected = i;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        self.status.clear();
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.key_normal(key),
            Mode::Prompt { kind, input } => self.key_prompt(kind, input, key),
            Mode::List { kind, items, selected } => self.key_list(kind, items, selected, key),
            Mode::Tags(form) => self.key_tags(form, key),
            Mode::ConfirmQuit => self.key_confirm_quit(key),
            Mode::ConfirmBack => self.key_confirm_back(key),
            Mode::Help => Outcome::Continue, // any key closes it
        }
    }

    fn key_normal(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = self.plan.findings.breaks.len().saturating_sub(1);
        let i = self.selected;
        match key.code {
            KeyCode::Char('c') if ctrl => return self.quit(),
            KeyCode::Char('r') if ctrl => self.redo(),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(PAGE),
            KeyCode::PageDown => self.selected = (self.selected + PAGE).min(last),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            KeyCode::Enter | KeyCode::Char('r') => {
                if let Some(b) = self.plan.findings.breaks.get(i) {
                    self.mode = Mode::Prompt { kind: Prompt::Rename(i), input: Input::new(&b.title) };
                }
            }
            KeyCode::Char('[') => self.edit(|p| ops::move_break(p, i, -1)),
            KeyCode::Char(']') => self.edit(|p| ops::move_break(p, i, 1)),
            KeyCode::Char('d') => self.edit(|p| ops::remove(p, i)),
            KeyCode::Char('a') => self.open_add(),
            KeyCode::Char('n') => self.open_silences(None),
            KeyCode::Char('m') => self.open_missing(),
            KeyCode::Char('t') => self.open_tags(tags::find_embedded_name),
            KeyCode::Char('g') => self.mode = Mode::Prompt { kind: Prompt::Jump, input: Input::new("") },
            KeyCode::Char('p') => {
                self.status = match write_clip(&self.plan, i + 1, None) {
                    Ok((_, line)) => line,
                    Err(e) => e,
                };
            }
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('s') => return Outcome::Save,
            KeyCode::Char('q') => return self.quit(),
            KeyCode::Char('b') if self.allow_back => {
                if self.changed() {
                    self.mode = Mode::ConfirmBack;
                } else {
                    return Outcome::Back;
                }
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
        Outcome::Continue
    }

    fn quit(&mut self) -> Outcome {
        if self.changed() {
            self.mode = Mode::ConfirmQuit;
            Outcome::Continue
        } else {
            Outcome::Discard
        }
    }

    fn key_confirm_quit(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Char('s') => Outcome::Save,
            KeyCode::Char('d') => Outcome::Discard,
            KeyCode::Esc => Outcome::Continue,
            _ => {
                self.mode = Mode::ConfirmQuit;
                Outcome::Continue
            }
        }
    }

    fn key_confirm_back(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Char('y') => Outcome::Back,
            KeyCode::Esc | KeyCode::Char('n') => Outcome::Continue,
            _ => {
                self.mode = Mode::ConfirmBack;
                Outcome::Continue
            }
        }
    }

    fn open_add(&mut self) {
        if self.plan.files.len() > 1 {
            let items: Vec<String> = self.plan.files.iter().map(|f| file_name(&f.path)).collect();
            self.mode = Mode::List { kind: ListKind::AddFile, items, selected: 0 };
        } else {
            self.mode = Mode::Prompt { kind: Prompt::AddTime(0), input: Input::new("") };
        }
    }

    fn open_silences(&mut self, title: Option<String>) {
        let spots = ops::unused_silences(&self.plan);
        if spots.is_empty() {
            self.status = "Every silence already starts a chapter.".into();
            return;
        }
        let items = spots
            .iter()
            .map(|&(file, at)| {
                let after = self.plan.findings.silences.iter().find(|s| ops::same((s.file, s.at), (file, at))).map_or("", |s| s.after.as_str());
                let preview: String = after.chars().take(50).collect();
                format!("{}  {}", place(&self.plan, file, at), preview)
            })
            .collect();
        self.mode = Mode::List { kind: ListKind::Silence { spots, title }, items, selected: 0 };
    }

    fn open_missing(&mut self) {
        if self.plan.findings.missing.is_empty() {
            self.status = "No chapters are missing.".into();
            return;
        }
        let items = self
            .plan
            .findings
            .missing
            .iter()
            .map(|m| format!("{}. {}{}", m.chapter + 1, m.title, if m.tried { "  (a re-scan found nothing)" } else { "" }))
            .collect();
        self.mode = Mode::List { kind: ListKind::Missing, items, selected: 0 };
    }

    fn key_prompt(&mut self, kind: Prompt, mut input: Input, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => match self.submit(&kind, &input.value()) {
                Ok(next) => self.mode = next,
                Err(e) => {
                    self.status = e;
                    self.mode = Mode::Prompt { kind, input };
                }
            },
            _ => {
                input.handle_key(key);
                self.mode = Mode::Prompt { kind, input };
            }
        }
        Outcome::Continue
    }

    // What happens when a prompt is accepted: the next mode, or why it was refused.
    fn submit(&mut self, kind: &Prompt, text: &str) -> Result<Mode, String> {
        match *kind {
            Prompt::Rename(i) => self.apply(|p| ops::rename(p, i, text)).map(|_| Mode::Normal),
            Prompt::AddTime(file) => {
                let at = parse_time(text)?;
                let duration = self.plan.files.get(file).map_or(0.0, |f| f.duration);
                if !(at >= 0.0 && at < duration) {
                    return Err(format!("Enter a time between 0:00:00 and {}.", clock(duration)));
                }
                Ok(Mode::Prompt { kind: Prompt::AddTitle(file, at), input: Input::new("New chapter") })
            }
            Prompt::AddTitle(file, at) => {
                self.apply(|p| ops::add(p, file, at, text))?;
                self.select_pos((file, at));
                Ok(Mode::Normal)
            }
            Prompt::Jump => self.jump(text).map(|_| Mode::Normal),
        }
    }

    // A time ("1:02:03", "0:40.5") selects the nearest chapter at or before it in the selected
    // chapter's file; a plain integer selects that chapter number.
    fn jump(&mut self, text: &str) -> Result<(), String> {
        let text = text.trim();
        let breaks = &self.plan.findings.breaks;
        if text.contains(':') || text.contains('.') {
            let at = parse_time(text)?;
            let file = breaks.get(self.selected).map_or(0, |b| b.file);
            let i = breaks
                .iter()
                .rposition(|b| b.file == file && b.at <= at)
                .ok_or("There is no chapter at or before that time in this file.")?;
            self.selected = i;
        } else {
            let n: usize = text.parse().map_err(|_| "Enter a time like 1:02:03 or a chapter number.".to_string())?;
            if n == 0 || n > breaks.len() {
                return Err(format!("There is no chapter {}.", n));
            }
            self.selected = n - 1;
        }
        Ok(())
    }

    fn key_list(&mut self, kind: ListKind, items: Vec<String>, mut selected: usize, key: KeyEvent) -> Outcome {
        let last = items.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => return Outcome::Continue,
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(last),
            KeyCode::Enter => {
                self.choose(kind, selected);
                return Outcome::Continue;
            }
            _ => {}
        }
        self.mode = Mode::List { kind, items, selected };
        Outcome::Continue
    }

    fn choose(&mut self, kind: ListKind, i: usize) {
        match kind {
            ListKind::AddFile => self.mode = Mode::Prompt { kind: Prompt::AddTime(i), input: Input::new("") },
            ListKind::Silence { spots, title } => {
                let (file, at) = spots[i];
                let initial = title.as_deref().unwrap_or("New chapter");
                self.mode = Mode::Prompt { kind: Prompt::AddTitle(file, at), input: Input::new(initial) };
            }
            ListKind::Missing => {
                let title = self.plan.findings.missing[i].title.clone();
                self.open_silences(Some(title));
            }
        }
    }

    fn key_tags(&mut self, mut form: Box<TagsForm>, key: KeyEvent) -> Outcome {
        match form.handle_key(key) {
            FormOutcome::Continue => self.mode = Mode::Tags(form),
            FormOutcome::Cancel => {}
            FormOutcome::Save => {
                if let Err(e) = self.apply(|p| form.apply(p)) {
                    form.error = e;
                    self.mode = Mode::Tags(form);
                }
            }
        }
        Outcome::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::edit::ops::sample_plan;
    use audiobook_splitter_rust::chapters::AudioFile;
    use audiobook_splitter_rust::plan::Missing;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn press(ed: &mut Editor, keys: &str) {
        for c in keys.chars() {
            ed.handle_key(k(KeyCode::Char(c)));
        }
    }

    // Replaces the text of the open prompt with `text` and presses Enter.
    fn answer(ed: &mut Editor, text: &str) {
        ed.handle_key(ctrl('u'));
        press(ed, text);
        ed.handle_key(k(KeyCode::Enter));
    }

    fn titles(ed: &Editor) -> Vec<&str> {
        ed.plan.findings.breaks.iter().map(|b| b.title.as_str()).collect()
    }

    fn at(ed: &Editor, i: usize) -> f64 {
        ed.plan.findings.breaks[i].at
    }

    fn editor() -> Editor {
        Editor::new(sample_plan())
    }

    #[test]
    fn navigation_stays_within_the_chapters() {
        let mut ed = editor();
        press(&mut ed, "jjj");
        assert_eq!(ed.selected, 2);
        press(&mut ed, "k");
        assert_eq!(ed.selected, 1);
        ed.handle_key(k(KeyCode::Home));
        assert_eq!(ed.selected, 0);
        ed.handle_key(k(KeyCode::Up));
        assert_eq!(ed.selected, 0);
        ed.handle_key(k(KeyCode::End));
        assert_eq!(ed.selected, 2);
        ed.handle_key(k(KeyCode::PageUp));
        assert_eq!(ed.selected, 0);
        ed.handle_key(k(KeyCode::PageDown));
        assert_eq!(ed.selected, 2);
    }

    #[test]
    fn rename_replaces_the_title() {
        let mut ed = editor();
        press(&mut ed, "j");
        ed.handle_key(k(KeyCode::Enter)); // the prompt starts with the current title
        answer(&mut ed, "Deux");
        assert_eq!(titles(&ed), ["One", "Deux", "Three"]);
        assert!(matches!(ed.mode, Mode::Normal));
        assert!(ed.changed());
    }

    #[test]
    fn a_blank_rename_is_refused_and_keeps_the_prompt_open() {
        let mut ed = editor();
        press(&mut ed, "r");
        answer(&mut ed, "   ");
        assert!(matches!(ed.mode, Mode::Prompt { .. }));
        assert!(ed.status.contains("needs a title"));
        assert_eq!(titles(&ed), ["One", "Two", "Three"]);
        ed.handle_key(k(KeyCode::Esc));
        assert!(matches!(ed.mode, Mode::Normal));
    }

    #[test]
    fn brackets_move_the_break_between_silences_and_undo_redo_follow() {
        let mut ed = editor();
        press(&mut ed, "j]");
        assert_eq!(at(&ed, 1), 45.0);
        press(&mut ed, "[[");
        assert_eq!(at(&ed, 1), 20.0, "45 -> 30 -> 20, one silence at a time");
        press(&mut ed, "u");
        assert_eq!(at(&ed, 1), 30.0);
        ed.handle_key(ctrl('r'));
        assert_eq!(at(&ed, 1), 20.0);
        press(&mut ed, "uuu");
        assert_eq!(at(&ed, 1), 30.0);
        assert!(!ed.changed(), "everything undone");
        press(&mut ed, "u");
        assert_eq!(ed.status, "Nothing to undo.");
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut ed = editor();
        press(&mut ed, "j][u"); // 30 -> 45 -> 30, then undo: back at 45 with a redo pending
        press(&mut ed, "["); // a new edit (45 -> 30) drops the pending redo
        ed.handle_key(ctrl('r'));
        assert_eq!(ed.status, "Nothing to redo.");
    }

    #[test]
    fn the_first_chapter_cannot_be_moved_or_removed_and_nothing_is_recorded() {
        let mut ed = editor();
        press(&mut ed, "d");
        assert!(ed.status.contains("can't be removed"));
        press(&mut ed, "]");
        assert!(ed.status.contains("can't be moved"));
        assert_eq!(titles(&ed), ["One", "Two", "Three"]);
        assert!(!ed.changed());
        press(&mut ed, "u");
        assert_eq!(ed.status, "Nothing to undo.");
    }

    #[test]
    fn remove_deletes_and_keeps_the_selection_valid() {
        let mut ed = editor();
        press(&mut ed, "jjd");
        assert_eq!(titles(&ed), ["One", "Two"]);
        assert_eq!(ed.selected, 1);
        press(&mut ed, "u");
        assert_eq!(titles(&ed), ["One", "Two", "Three"]);
        assert_eq!(ed.selected, 2, "undo restores the selection");
    }

    #[test]
    fn add_at_a_time_asks_for_the_time_then_the_title() {
        let mut ed = editor();
        press(&mut ed, "a");
        answer(&mut ed, "0:45");
        assert!(matches!(ed.mode, Mode::Prompt { kind: Prompt::AddTitle(0, _), .. }));
        answer(&mut ed, "Fresh");
        assert_eq!(titles(&ed), ["One", "Two", "Fresh", "Three"]);
        assert_eq!(ed.selected, 2, "the new chapter is selected");
    }

    #[test]
    fn add_at_a_time_refuses_bad_times_and_duplicates() {
        let mut ed = editor();
        press(&mut ed, "a");
        answer(&mut ed, "5:00");
        assert!(matches!(ed.mode, Mode::Prompt { kind: Prompt::AddTime(0), .. }));
        assert!(ed.status.contains("between 0:00:00"));
        answer(&mut ed, "soon");
        assert!(ed.status.contains("Enter a time like"));
        answer(&mut ed, "0:30");
        answer(&mut ed, "Dup");
        assert!(ed.status.contains("already a chapter"));
        assert_eq!(titles(&ed), ["One", "Two", "Three"]);
    }

    #[test]
    fn with_several_files_add_asks_which_file_first() {
        let mut p = sample_plan();
        let second = AudioFile { path: "b.mp3".into(), silences: vec![], duration: 50.0 };
        p.files.push(second);
        let mut ed = Editor::new(p);
        press(&mut ed, "a");
        assert!(matches!(ed.mode, Mode::List { kind: ListKind::AddFile, .. }));
        ed.handle_key(k(KeyCode::Down));
        ed.handle_key(k(KeyCode::Enter));
        answer(&mut ed, "0:20");
        answer(&mut ed, "In file two");
        let b = ed.plan.findings.breaks.last().unwrap();
        assert_eq!((b.file, b.at, b.title.as_str()), (1, 20.0, "In file two"));
    }

    #[test]
    fn add_at_a_silence_lists_the_unused_ones() {
        let mut ed = editor();
        press(&mut ed, "n");
        let Mode::List { kind: ListKind::Silence { spots, .. }, items, .. } = &ed.mode else { panic!("expected the silence list") };
        assert_eq!(spots.iter().map(|s| s.1).collect::<Vec<_>>(), [10.0, 20.0, 45.0, 80.0]);
        assert_eq!(items.len(), 4);
        ed.handle_key(k(KeyCode::Down));
        ed.handle_key(k(KeyCode::Down));
        ed.handle_key(k(KeyCode::Enter));
        answer(&mut ed, "At the gap");
        assert_eq!(titles(&ed), ["One", "Two", "At the gap", "Three"]);
        assert_eq!(at(&ed, 2), 45.0);
    }

    #[test]
    fn the_missing_view_adds_a_chapter_with_its_title_preset() {
        let mut p = sample_plan();
        p.findings.missing.push(Missing { chapter: 3, title: "Four".into(), tried: false });
        let mut ed = Editor::new(p);
        press(&mut ed, "m");
        assert!(matches!(ed.mode, Mode::List { kind: ListKind::Missing, .. }));
        ed.handle_key(k(KeyCode::Enter)); // pick the missing chapter: now choose a silence
        ed.handle_key(k(KeyCode::Enter)); // the first unused silence, 10 s
        let Mode::Prompt { input, .. } = &ed.mode else { panic!("expected the title prompt") };
        assert_eq!(input.value(), "Four");
        ed.handle_key(k(KeyCode::Enter));
        assert_eq!((ed.plan.findings.breaks[1].title.as_str(), at(&ed, 1)), ("Four", 10.0));
    }

    #[test]
    fn views_with_nothing_to_show_say_so() {
        let mut ed = editor();
        press(&mut ed, "m");
        assert_eq!(ed.status, "No chapters are missing.");
        for t in [10.0, 20.0, 45.0, 80.0] {
            crate::cli::edit::ops::add(&mut ed.plan, 0, t, "x").unwrap();
        }
        press(&mut ed, "n");
        assert_eq!(ed.status, "Every silence already starts a chapter.");
        assert!(matches!(ed.mode, Mode::Normal));
    }

    #[test]
    fn jump_takes_a_time_or_a_chapter_number() {
        let mut ed = editor();
        press(&mut ed, "g");
        answer(&mut ed, "1:00");
        assert_eq!(ed.selected, 2);
        press(&mut ed, "g");
        answer(&mut ed, "0:40");
        assert_eq!(ed.selected, 1, "the nearest chapter at or before 40 s");
        press(&mut ed, "g");
        answer(&mut ed, "3");
        assert_eq!(ed.selected, 2);
        press(&mut ed, "g");
        answer(&mut ed, "9");
        assert!(ed.status.contains("There is no chapter 9"));
        assert!(matches!(ed.mode, Mode::Prompt { .. }), "a refused jump keeps the prompt open");
        answer(&mut ed, "x");
        assert!(ed.status.contains("Enter a time like"));
    }

    #[test]
    fn quitting_asks_only_when_there_are_unsaved_changes() {
        let mut ed = editor();
        assert!(matches!(ed.handle_key(k(KeyCode::Char('q'))), Outcome::Discard));
        let mut ed = editor();
        press(&mut ed, "j]");
        assert!(matches!(ed.handle_key(k(KeyCode::Char('q'))), Outcome::Continue));
        assert!(matches!(ed.mode, Mode::ConfirmQuit));
        ed.handle_key(k(KeyCode::Esc));
        assert!(matches!(ed.mode, Mode::Normal));
        ed.handle_key(ctrl('c'));
        assert!(matches!(ed.mode, Mode::ConfirmQuit));
        assert!(matches!(ed.handle_key(k(KeyCode::Char('d'))), Outcome::Discard));
        let mut ed = editor();
        press(&mut ed, "j]");
        press(&mut ed, "q");
        assert!(matches!(ed.handle_key(k(KeyCode::Char('s'))), Outcome::Save));
    }

    #[test]
    fn s_saves_and_help_opens_and_closes() {
        let mut ed = editor();
        press(&mut ed, "?");
        assert!(matches!(ed.mode, Mode::Help));
        ed.handle_key(k(KeyCode::Char('x')));
        assert!(matches!(ed.mode, Mode::Normal));
        assert!(matches!(ed.handle_key(k(KeyCode::Char('s'))), Outcome::Save));
    }

    #[test]
    fn back_only_works_when_the_app_allows_it() {
        let mut ed = editor();
        press(&mut ed, "b");
        assert!(matches!(ed.mode, Mode::Normal));
        ed.allow_back = true;
        assert!(matches!(ed.handle_key(k(KeyCode::Char('b'))), Outcome::Back), "nothing to lose: back at once");
        press(&mut ed, "j]");
        assert!(matches!(ed.handle_key(k(KeyCode::Char('b'))), Outcome::Continue));
        assert!(matches!(ed.mode, Mode::ConfirmBack));
        ed.handle_key(k(KeyCode::Esc));
        assert!(matches!(ed.mode, Mode::Normal));
        press(&mut ed, "b");
        assert!(matches!(ed.handle_key(k(KeyCode::Char('y'))), Outcome::Back));
    }

    #[test]
    fn preview_reports_a_missing_audio_file_in_the_status_line() {
        let mut ed = editor(); // a.mp3 does not exist
        press(&mut ed, "jp");
        assert!(!ed.status.is_empty());
        assert!(!ed.status.starts_with("Wrote"));
    }

    #[test]
    fn the_embedded_cover_is_looked_up_once_per_editor() {
        let mut ed = editor();
        let mut calls = 0;
        for _ in 0..3 {
            ed.open_tags(|_| {
                calls += 1;
                Some("a.mp3".to_string())
            });
            assert!(matches!(ed.mode, Mode::Tags(_)));
            ed.handle_key(k(KeyCode::Esc));
        }
        assert_eq!(calls, 1);
        assert_eq!(ed.embedded_cover, Some(Some("a.mp3".to_string())));
        // A "no picture" answer is cached too.
        let mut ed = editor();
        let mut calls = 0;
        for _ in 0..2 {
            ed.open_tags(|_| {
                calls += 1;
                None
            });
        }
        assert_eq!(calls, 1);
    }

    #[test]
    fn the_tags_form_opens_and_esc_closes_it_without_changes() {
        let mut ed = editor();
        press(&mut ed, "t");
        assert!(matches!(ed.mode, Mode::Tags(_)));
        ed.handle_key(k(KeyCode::Esc));
        assert!(matches!(ed.mode, Mode::Normal));
        assert!(!ed.changed());
    }
}
