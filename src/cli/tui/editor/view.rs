// Drawing the editor: the chapter table, the detail pane, the footer and the popups.
use super::*;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap};
use ratatui::Frame;
use super::super::hints::{key_style, wrap_hints};

const HELP: &[(&str, &str)] = &[
    ("j/k, arrows, Pg, Home/End", "move the selection"),
    ("Enter, r", "rename the chapter"),
    ("[  ]", "move break to prev/next silence"),
    ("d", "remove the chapter"),
    ("a / n", "add at a time / at a silence"),
    ("m", "chapters the EPUB match missed"),
    ("t", "tags, file naming and cover art"),
    ("g", "jump to a time (1:02:03) or a chapter number"),
    ("p", "write a preview clip of the selected break"),
    ("u / Ctrl-r", "undo / redo"),
    ("s / q, Ctrl-c", "save / quit (asks if unsaved)"),
    ("b", "back to Setup (only in `tui`)"),
    ("?", "this help"),
];

pub(crate) fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

impl Editor {
    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        let detail_h = match area.height {
            0..=14 => 1,
            15..=19 => 4,
            _ => 7,
        };
        let mut keys = self.footer_keys(area.width as usize);
        // The table keeps at least 3 rows; a terminal too short for the full list gets the compact one.
        if area.height < 12 || keys.len() as u16 + 1 + detail_h + 3 > area.height {
            keys = wrap_hints(&[("?", "help"), ("s", self.finish_verb()), ("q", "quit")], "  ", area.width as usize);
        }
        let footer_h = keys.len() as u16 + 1;
        let [table_area, detail_area, footer_area] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(detail_h), Constraint::Length(footer_h)]).areas(area);
        self.render_table(f, table_area);
        self.render_detail(f, detail_area);
        self.render_footer(f, footer_area, keys);
        self.render_popup(f, area);
    }

    fn render_table(&mut self, f: &mut Frame, area: Rect) {
        let multi = self.plan.files.len() > 1;
        let where_w = if multi { 30 } else { 9 };
        let rows: Vec<Row> = self
            .plan
            .findings
            .breaks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let score = b.score.map_or("-".to_string(), |s| format!("{:.2}", s));
                let fixed = if ops::is_fixed(b) { "*" } else { "" };
                Row::new(vec![format!("{}{}", i + 1, fixed), place(&self.plan, b.file, b.at), b.title.clone(), score])
            })
            .collect();
        let title = format!(" Chapters ({}){} ", rows.len(), if self.changed() { " *" } else { "" });
        let table = Table::new(rows, [Constraint::Length(5), Constraint::Length(where_w), Constraint::Fill(1), Constraint::Length(5)])
            .header(Row::new(vec!["#", "Where", "Title", "Match"]).style(Style::new().add_modifier(Modifier::BOLD)))
            .block(Block::bordered().title(title))
            .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol(">");
        self.table.select(Some(self.selected));
        f.render_stateful_widget(table, area, &mut self.table);
    }

    fn render_detail(&self, f: &mut Frame, area: Rect) {
        let Some(b) = self.plan.findings.breaks.get(self.selected) else { return };
        if area.height < 3 {
            // Too short for a bordered box: one summary line.
            let line = Line::from(vec![
                Span::styled(format!("Chapter {}: ", self.selected + 1), Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(format!("{}  at {}", b.title, place(&self.plan, b.file, b.at))),
            ]);
            f.render_widget(Paragraph::new(line), area);
            return;
        }
        let mut lines = vec![Line::from(vec![
            Span::styled(format!("Chapter {}: ", self.selected + 1), Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{}  at {}", b.title, place(&self.plan, b.file, b.at))),
        ])];
        if ops::is_fixed(b) {
            lines.push(Line::raw("The first chapter starts the book and can't be moved or removed."));
        }
        match ops::heard(&self.plan, self.selected) {
            Some((before, after)) => {
                lines.push(Line::raw(format!("Heard before: {}", before)));
                lines.push(Line::raw(format!("Heard after:  {}", after)));
            }
            None => lines.push(Line::raw("Nothing was transcribed here (no EPUB, or the break was added by hand).")),
        }
        let silence = |dir| ops::neighbour(&self.plan, self.selected, dir).map_or("none".to_string(), |(_, at)| clock(at));
        lines.push(Line::raw(format!("Previous silence: {}   Next silence: {}", silence(-1), silence(1))));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(Block::bordered().title(" Detail ")), area);
    }

    fn footer_keys(&self, width: usize) -> Vec<Line<'static>> {
        let v = self.finish_verb();
        wrap_hints(&[("j/k", "move"), ("r", "rename"), ("[ ]", "move break"), ("d", "remove"), ("a", "add"), ("n", "silence"), ("m", "missing"), ("t", "tags"), ("g", "jump"), ("p", "preview"), ("u", "undo"), ("s", v), ("q", "quit"), ("?", "help")], "  ", width)
    }

    fn render_footer(&self, f: &mut Frame, area: Rect, mut lines: Vec<Line<'static>>) {
        lines.push(Line::from(Span::styled(self.status.clone(), Style::new().fg(Color::Yellow))));
        f.render_widget(Paragraph::new(lines), area);
    }

    fn render_popup(&mut self, f: &mut Frame, area: Rect) {
        match &mut self.mode {
            Mode::Normal => {}
            Mode::Prompt { kind, input } => {
                let title = match kind {
                    Prompt::Rename(_) => " New title ",
                    Prompt::AddTime(_) => " Start time (h:mm:ss) ",
                    Prompt::AddTitle(..) => " Chapter title ",
                    Prompt::Jump => " Jump to a time or a chapter number ",
                };
                let rect = centered(area, 60, 3);
                let inner_w = rect.width.saturating_sub(2) as usize;
                let (text, col) = input.view(inner_w);
                f.render_widget(Clear, rect);
                f.render_widget(Paragraph::new(text).block(Block::bordered().title(title)), rect);
                f.set_cursor_position((rect.x + 1 + col as u16, rect.y + 1));
            }
            Mode::List { kind, items, selected } => {
                let title = match kind {
                    ListKind::AddFile => " Which file? ",
                    ListKind::Silence { .. } => " Start a chapter at ",
                    ListKind::Missing => " Chapters with no break (Enter: choose a silence) ",
                };
                let rect = centered(area, 70, (items.len().min(u16::MAX as usize - 2) as u16).saturating_add(2).min(area.height.saturating_sub(2)).max(3));
                let mut state = ListState::default();
                state.select(Some(*selected));
                let list = List::new(items.iter().map(|s| ListItem::new(s.as_str())))
                    .block(Block::bordered().title(title))
                    .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
                    .highlight_symbol(">");
                f.render_widget(Clear, rect);
                f.render_stateful_widget(list, rect, &mut state);
            }
            Mode::Tags(form) => form.render(f, area),
            Mode::ConfirmQuit => {
                let first = if self.allow_back { "split now" } else { "save and continue" };
                confirm(f, area, " Unsaved changes ", None, &[("s", first), ("d", "discard"), ("Esc", "keep editing")], "    ")
            }
            Mode::ConfirmBack => {
                confirm(f, area, " Go back? ", Some("Edits will be lost."), &[("y", "go back"), ("Esc", "stay")], "   ")
            }
            Mode::Help => {
                let rect = centered(area, 76, HELP.len() as u16 + 2);
                let splits = self.allow_back;
                let lines: Vec<Line> = HELP
                    .iter()
                    .map(|(k, d)| {
                        let d = if splits && *k == "s / q, Ctrl-c" { "split / quit (asks if unsaved)" } else { d };
                        Line::from(vec![Span::styled(format!("{:<26}", k), key_style()), Span::raw(d)])
                    })
                    .collect();
                f.render_widget(Clear, rect);
                f.render_widget(Paragraph::new(lines).block(Block::bordered().title(" Keys (any key closes) ")), rect);
            }
        }
    }
}

// A popup of an optional sentence and the command hints, wrapped to the popup width.
pub(crate) fn confirm(f: &mut Frame, area: Rect, title: &str, prose: Option<&str>, pairs: &[(&str, &str)], sep: &str) {
    let w = 72.min(area.width);
    let mut lines: Vec<Line> = prose.map(Line::raw).into_iter().collect();
    lines.extend(wrap_hints(pairs, sep, w.saturating_sub(2) as usize));
    let rect = centered(area, w, lines.len() as u16 + 2);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(title)), rect);
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::cli::edit::ops::sample_plan;
    use ratatui::backend::TestBackend;
    use ratatui::style::{Color, Modifier};
    use ratatui::Terminal;

    fn draw(ed: &mut Editor, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| ed.render(f)).unwrap();
        let buf = t.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn k(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn shows_the_chapters_the_selection_and_the_keys() {
        let mut ed = Editor::new(sample_plan());
        ed.handle_key(k('j'));
        let screen = draw(&mut ed, 80, 24);
        for want in ["Chapters (3)", "One", "Two", "Three", "0:00:30", "q quit"] {
            assert!(screen.contains(want), "missing {:?} in\n{}", want, screen);
        }
        let marked: Vec<&str> = screen.lines().filter(|l| l.contains('>')).collect();
        assert_eq!(marked.len(), 1);
        assert!(marked[0].contains("Two"), "the marker is on the selected row: {:?}", marked);
    }

    #[test]
    fn the_detail_pane_shows_what_was_heard_or_says_nothing_was() {
        let mut p = sample_plan();
        p.findings.silences[3].before = "the end of the last chapter".into();
        p.findings.silences[3].after = "chapter two begins here".into();
        let mut ed = Editor::new(p);
        ed.handle_key(k('j')); // the break at 30 s sits on the silence at 30 s
        let screen = draw(&mut ed, 80, 24);
        assert!(screen.contains("chapter two begins here"), "{}", screen);
        assert!(screen.contains("the end of the last chapter"), "{}", screen);
        let mut plain = Editor::new(sample_plan());
        plain.handle_key(k('j'));
        assert!(draw(&mut plain, 80, 24).contains("Nothing was transcribed here"));
    }

    #[test]
    fn the_first_chapter_is_marked_fixed_and_changes_show_in_the_title() {
        let mut ed = Editor::new(sample_plan());
        assert!(draw(&mut ed, 80, 24).contains("starts the book"));
        ed.handle_key(k('j'));
        ed.handle_key(k(']'));
        assert!(draw(&mut ed, 80, 24).contains("Chapters (3) *"), "unsaved changes are flagged");
    }

    #[test]
    fn popups_draw_over_the_table() {
        let mut ed = Editor::new(sample_plan());
        ed.handle_key(k('r'));
        let screen = draw(&mut ed, 80, 24);
        assert!(screen.contains("New title") && screen.contains("One"), "{}", screen);
        ed.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ed.handle_key(k('n'));
        assert!(draw(&mut ed, 80, 24).contains("Start a chapter at"));
        ed.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ed.handle_key(k('?'));
        let help = draw(&mut ed, 80, 24);
        assert!(help.contains("undo") && help.contains("jump") && help.contains("Home/End"), "{}", help);
        ed.handle_key(k('x'));
        ed.handle_key(k('j'));
        ed.handle_key(k(']'));
        ed.handle_key(k('q'));
        assert!(draw(&mut ed, 80, 24).contains("Unsaved changes"));
    }

    #[test]
    fn the_status_line_shows_refusals() {
        let mut ed = Editor::new(sample_plan());
        ed.handle_key(k('d'));
        assert!(draw(&mut ed, 80, 24).contains("can't be removed"));
    }

    #[test]
    fn the_minimum_size_and_long_titles_do_not_panic() {
        let mut p = sample_plan();
        p.findings.breaks[1].title = "A very long chapter title ".repeat(12);
        let mut ed = Editor::new(p);
        for (w, h) in [(60, 15), (60, 12), (40, 8), (20, 5), (200, 60)] {
            let screen = draw(&mut ed, w, h);
            assert!(!screen.is_empty());
        }
        let small = draw(&mut ed, 60, 15);
        assert!(small.contains("One") && small.contains("? help"), "{}", small);
        ed.handle_key(k('r'));
        draw(&mut ed, 60, 15);
        ed.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ed.handle_key(k('?'));
        draw(&mut ed, 60, 15);
    }

    #[test]
    fn the_keys_say_split_in_the_app_and_save_otherwise() {
        let mut ed = Editor::new(sample_plan());
        assert!(draw(&mut ed, 80, 24).contains("s save"));
        ed.handle_key(k('?'));
        assert!(draw(&mut ed, 80, 24).contains("save / quit (asks if unsaved)"));
        ed.handle_key(k('x'));
        ed.allow_back = true;
        let screen = draw(&mut ed, 80, 24);
        assert!(screen.contains("s split") && !screen.contains("s save"), "{}", screen);
        ed.handle_key(k('?'));
        assert!(draw(&mut ed, 80, 24).contains("split / quit (asks if unsaved)"));
        ed.handle_key(k('x'));
        ed.handle_key(k('j'));
        ed.handle_key(k(']'));
        ed.handle_key(k('q'));
        let screen = draw(&mut ed, 80, 24);
        assert!(screen.contains("s split now"), "{}", screen);
    }

    #[test]
    fn the_confirm_popups_fit_at_60x15() {
        let mut ed = Editor::new(sample_plan());
        ed.allow_back = true;
        ed.handle_key(k('j'));
        ed.handle_key(k(']'));
        ed.handle_key(k('b'));
        let screen = draw(&mut ed, 60, 15);
        assert!(screen.contains("Esc stay"), "{}", screen);
        ed.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ed.handle_key(k('q'));
        assert!(draw(&mut ed, 60, 15).contains("Esc keep editing"));
    }

    #[test]
    fn the_help_popup_fits_at_60x15_with_the_last_keys() {
        let mut ed = Editor::new(sample_plan());
        ed.handle_key(k('?'));
        let screen = draw(&mut ed, 60, 15);
        for want in ["undo", "jump", "back to Setup", "quit", "move break to prev/next silence", "add at a time / at a silence", "save / quit (asks if unsaved)"] {
            assert!(screen.contains(want), "missing {:?} in\n{}", want, screen);
        }
    }

    #[test]
    fn a_short_terminal_shows_a_one_line_summary_instead_of_an_empty_detail_box() {
        let mut ed = Editor::new(sample_plan());
        ed.handle_key(k('j'));
        let screen = draw(&mut ed, 60, 12);
        assert!(screen.contains("Chapter 2: Two"), "{}", screen);
    }

    #[test]
    fn a_long_list_scrolls_to_keep_the_selection_visible() {
        let mut p = sample_plan();
        p.files[0].duration = 10_000.0;
        for n in 1..=40 {
            crate::cli::edit::ops::add(&mut p, 0, 100.0 + n as f64 * 10.0, &format!("Extra {}", n)).unwrap();
        }
        let mut ed = Editor::new(p);
        ed.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let screen = draw(&mut ed, 80, 24);
        assert!(screen.contains("Extra 40") && !screen.contains("Extra 1 "), "{}", screen);
        ed.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert!(draw(&mut ed, 80, 24).contains("One"));
    }

    fn assert_key_style(ed: &mut Editor, w: u16, h: u16, key_and_desc: &str, key_len: u16) {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| ed.render(f)).unwrap();
        let buf = t.backend().buffer();
        let (x, y) = crate::cli::tui::hints::find_text(buf, key_and_desc).unwrap_or_else(|| panic!("no {:?}", key_and_desc));
        let key = buf[(x, y)].style();
        assert_eq!(key.fg, Some(Color::Cyan), "{}", key_and_desc);
        assert!(key.add_modifier.contains(Modifier::BOLD), "{}", key_and_desc);
        let desc = buf[(x + key_len + 1, y)].style();
        assert_ne!(desc.fg, Some(Color::Cyan), "{}", key_and_desc);
        assert!(!desc.add_modifier.contains(Modifier::BOLD), "{}", key_and_desc);
    }

    #[test]
    fn the_footer_and_confirm_popup_keys_are_bold_cyan() {
        let mut ed = Editor::new(sample_plan());
        assert_key_style(&mut ed, 80, 24, "q quit", 1);
        ed.handle_key(k('j'));
        ed.handle_key(k(']'));
        ed.handle_key(k('q'));
        assert_key_style(&mut ed, 80, 24, "Esc keep editing", 3);
    }

    const COMMANDS: &[&str] = &["j/k move", "r rename", "[ ] move break", "d remove", "a add", "n silence", "m missing", "t tags", "g jump", "p preview", "u undo", "s save", "q quit", "? help"];

    fn rows(ed: &mut Editor, w: u16, h: u16) -> Vec<String> {
        draw(ed, w, h).lines().map(String::from).collect()
    }

    #[test]
    fn the_footer_wraps_between_commands_and_never_cuts_one() {
        for w in [60u16, 80, 120] {
            let mut ed = Editor::new(sample_plan());
            let r = rows(&mut ed, w, 24);
            let foot = &r[r.len().saturating_sub(6)..];
            for want in COMMANDS {
                assert!(foot.iter().any(|l| l.contains(want)), "{:?} not whole on one line at {}:\n{}", want, w, foot.join("\n"));
            }
            let used = foot.iter().filter(|l| !l.trim().is_empty()).count();
            if w == 60 {
                assert!(used > 1, "wrapped at 60:\n{}", foot.join("\n"));
            }
            ed.allow_back = true;
            let r = rows(&mut ed, w, 24);
            assert!(r[r.len().saturating_sub(6)..].iter().any(|l| l.contains("s split")));
        }
    }

    #[test]
    fn a_short_terminal_falls_back_to_the_compact_footer_and_keeps_a_table_row() {
        let mut ed = Editor::new(sample_plan());
        let screen = draw(&mut ed, 60, 10);
        assert!(screen.contains("? help") && screen.contains("q quit"), "{}", screen);
        assert!(screen.contains("One"), "{}", screen);
    }

    #[test]
    fn the_confirm_popup_wraps_whole_commands_on_a_narrow_terminal() {
        let mut ed = Editor::new(sample_plan());
        ed.handle_key(k('j'));
        ed.handle_key(k(']'));
        ed.handle_key(k('q'));
        let r = rows(&mut ed, 40, 15);
        for want in ["s save and continue", "d discard", "Esc keep editing"] {
            assert!(r.iter().any(|l| l.contains(want)), "{:?} not whole:\n{}", want, r.join("\n"));
        }
    }
}
