// A single-line text field: characters, cursor movement and deletion. Columns are counted
// in characters, so wide (CJK) glyphs can look misaligned; that is acceptable here.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct Input {
    text: Vec<char>,
    cursor: usize,
}

impl Input {
    pub fn new(initial: &str) -> Input {
        let text: Vec<char> = initial.chars().collect();
        Input { cursor: text.len(), text }
    }

    pub fn value(&self) -> String {
        self.text.iter().collect()
    }

    #[cfg(test)]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    // Applies an editing key; false means the key isn't one (Enter, Esc, Tab, Ctrl-C, ...).
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('u') if ctrl => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                self.text.insert(self.cursor, c);
                self.cursor += 1;
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.cursor);
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.text.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            _ => return false,
        }
        true
    }

    // The part of the text to draw in `width` columns, scrolled so the cursor stays visible,
    // and the cursor's column within it.
    pub fn view(&self, width: usize) -> (String, usize) {
        if width == 0 {
            return (String::new(), 0);
        }
        let start = if self.cursor >= width { self.cursor + 1 - width } else { 0 };
        let end = (start + width).min(self.text.len());
        (self.text[start..end].iter().collect(), self.cursor - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(input: &mut Input, text: &str) {
        for c in text.chars() {
            input.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut i = Input::new("ac");
        i.handle_key(key(KeyCode::Left));
        typed(&mut i, "b");
        assert_eq!((i.value().as_str(), i.cursor()), ("abc", 2));
    }

    #[test]
    fn backspace_and_delete_remove_around_the_cursor() {
        let mut i = Input::new("abcd");
        i.handle_key(key(KeyCode::Left));
        i.handle_key(key(KeyCode::Backspace));
        assert_eq!(i.value(), "abd");
        i.handle_key(key(KeyCode::Delete));
        assert_eq!(i.value(), "ab");
        i.handle_key(key(KeyCode::Delete)); // at the end: nothing to delete
        i.handle_key(key(KeyCode::Home));
        i.handle_key(key(KeyCode::Backspace)); // at the start: nothing to delete
        assert_eq!((i.value().as_str(), i.cursor()), ("ab", 0));
    }

    #[test]
    fn cursor_movement_stays_in_bounds() {
        let mut i = Input::new("ab");
        i.handle_key(key(KeyCode::Right));
        assert_eq!(i.cursor(), 2);
        i.handle_key(key(KeyCode::Home));
        i.handle_key(key(KeyCode::Left));
        assert_eq!(i.cursor(), 0);
        i.handle_key(key(KeyCode::End));
        assert_eq!(i.cursor(), 2);
    }

    #[test]
    fn unicode_counts_characters_not_bytes() {
        let mut i = Input::new("héllo");
        assert_eq!(i.cursor(), 5);
        i.handle_key(key(KeyCode::Left));
        i.handle_key(key(KeyCode::Backspace));
        assert_eq!(i.value(), "hélo");
        let mut j = Input::new("");
        typed(&mut j, "日本");
        j.handle_key(key(KeyCode::Backspace));
        assert_eq!(j.value(), "日");
    }

    #[test]
    fn ctrl_u_clears_and_other_keys_are_not_edits() {
        let mut i = Input::new("abc");
        assert!(i.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
        assert_eq!(i.value(), "");
        assert!(!i.handle_key(key(KeyCode::Enter)));
        assert!(!i.handle_key(key(KeyCode::Esc)));
        assert!(!i.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), "ctrl-c is not text");
    }

    #[test]
    fn view_scrolls_to_keep_the_cursor_visible() {
        let i = Input::new("abcdefghij");
        assert_eq!(i.view(20), ("abcdefghij".to_string(), 10));
        assert_eq!(i.view(5), ("ghij".to_string(), 4), "the cursor sits on the last visible column, after the final char");
        let mut j = Input::new("abcdefghij");
        j.handle_key(key(KeyCode::Home));
        assert_eq!(j.view(5), ("abcde".to_string(), 0));
    }
}
