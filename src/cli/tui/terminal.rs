// Entering and leaving the full-screen terminal safely, and deciding whether to.
use ratatui::DefaultTerminal;
use std::io::IsTerminal;

// Smaller than this and a full-screen layout stops being useful.
pub const MIN_SIZE: (u16, u16) = (60, 15);

// Whether a full-screen UI is a good idea here.
pub fn use_tui(simple: bool, term: Option<&str>, size: (u16, u16), is_tty: bool) -> bool {
    !simple && is_tty && term.is_some_and(|t| !t.is_empty() && t != "dumb") && size.0 >= MIN_SIZE.0 && size.1 >= MIN_SIZE.1
}

// `use_tui` for the real environment. Windows terminals don't set TERM, so there it counts as fine.
pub fn suitable(simple: bool) -> bool {
    let size = ratatui::crossterm::terminal::size().unwrap_or((0, 0));
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let term = std::env::var("TERM").ok().or_else(|| cfg!(windows).then(|| "windows".to_string()));
    use_tui(simple, term.as_deref(), size, tty)
}

// Raw mode and the alternate screen, undone on drop. `try_init` also installs a panic hook
// that restores the terminal first, so a crash never leaves an SSH session unusable.
pub struct TerminalGuard {
    pub terminal: DefaultTerminal,
}

impl TerminalGuard {
    pub fn new() -> Result<TerminalGuard, String> {
        ratatui::try_init().map(|terminal| TerminalGuard { terminal }).map_err(|e| e.to_string())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_tui_needs_a_real_big_enough_terminal() {
        let ok = |simple, term, size, tty| use_tui(simple, term, size, tty);
        assert!(ok(false, Some("xterm-256color"), (80, 24), true));
        assert!(ok(false, Some("screen"), (60, 15), true), "the minimum size is allowed");
        assert!(!ok(true, Some("xterm"), (80, 24), true), "--simple");
        assert!(!ok(false, Some("xterm"), (80, 24), false), "not a TTY");
        assert!(!ok(false, None, (80, 24), true), "TERM unset");
        assert!(!ok(false, Some(""), (80, 24), true), "TERM empty");
        assert!(!ok(false, Some("dumb"), (80, 24), true), "TERM dumb");
        assert!(!ok(false, Some("xterm"), (59, 24), true), "too narrow");
        assert!(!ok(false, Some("xterm"), (80, 14), true), "too short");
    }
}
