// The full-screen terminal UI: the chapter editor (used by `edit` and `run`) and the `tui` app.
pub mod browser;
pub mod editor;
pub mod input;
pub mod tags;
pub mod terminal;

use audiobook_splitter_rust::plan::Plan;
use editor::{Editor, Outcome};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use terminal::TerminalGuard;

// Runs the full-screen chapter editor on `plan`. Returns whether to save; on true `*plan`
// holds the edited plan. The terminal is restored before this returns.
pub fn edit(plan: &mut Plan, mut guard: TerminalGuard) -> Result<bool, String> {
    let mut ed = Editor::new(plan.clone());
    let outcome = loop {
        guard.terminal.draw(|f| ed.render(f)).map_err(|e| e.to_string())?;
        match event::read().map_err(|e| e.to_string())? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match ed.handle_key(key) {
                Outcome::Continue | Outcome::Back => {}
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
