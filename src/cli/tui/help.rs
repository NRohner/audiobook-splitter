// A help popup for the screens that have no key table of their own.
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

pub const SETUP_KEYS: &[(&str, &str)] = &[
    ("Tab, Shift-Tab, Up, Down", "move between the fields"),
    ("Enter", "browse for a path (on the first three rows)"),
    ("Space, Left, Right", "turn the cache on or off"),
    ("Ctrl-U", "clear the field"),
    ("Ctrl-A", "start the analysis"),
    ("Esc, Ctrl-c", "quit"),
    ("F1", "this help"),
];

pub const PROGRESS_KEYS: &[(&str, &str)] = &[
    ("q, Ctrl-c", "quit (asks first; the job cannot be cancelled)"),
    ("?", "this help"),
];

pub const DONE_KEYS: &[(&str, &str)] = &[
    ("n", "another book (back to Setup)"),
    ("q, Enter, Esc", "quit"),
    ("?", "this help"),
];

pub fn render_help(f: &mut Frame, area: Rect, title: &str, keys: &[(&str, &str)]) {
    let rect = super::editor::centered(area, 70, keys.len() as u16 + 2);
    let lines: Vec<Line> = keys
        .iter()
        .map(|(k, d)| Line::from(vec![Span::styled(format!("{:<26}", k), Style::new().add_modifier(Modifier::BOLD)), Span::raw(*d)]))
        .collect();
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(format!(" {} (any key closes) ", title))), rect);
}
