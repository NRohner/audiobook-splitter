// Key hint lines: the key names stand out in bold cyan, the descriptions stay in the normal style.
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub fn key_style() -> Style {
    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

// Each pair is the key, a space and the description (just the key when the description is empty).
pub fn hint_spans_sep(pairs: &[(&str, &str)], sep: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, (key, desc)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(sep.to_string()));
        }
        spans.push(Span::styled(key.to_string(), key_style()));
        if !desc.is_empty() {
            spans.push(Span::raw(format!(" {}", desc)));
        }
    }
    spans
}

pub fn hint_spans(pairs: &[(&str, &str)]) -> Vec<Span<'static>> {
    hint_spans_sep(pairs, "  ")
}

pub fn hint_line(pairs: &[(&str, &str)]) -> Line<'static> {
    Line::from(hint_spans(pairs))
}

pub fn hint_line_sep(pairs: &[(&str, &str)], sep: &str) -> Line<'static> {
    Line::from(hint_spans_sep(pairs, sep))
}

// Test helper: the buffer cell where `needle` first starts in a row.
#[cfg(test)]
pub fn find_text(buf: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
    (0..buf.area.height).find_map(|y| {
        let row: Vec<String> = (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect();
        (0..row.len()).find(|&x| row[x..].concat().starts_with(needle)).map(|x| (x as u16, y))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }
    fn is_key(s: &Span) -> bool {
        s.style.fg == Some(Color::Cyan) && s.style.add_modifier.contains(Modifier::BOLD)
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(plain(&hint_line(&[("Enter", "open"), ("Esc", "cancel")])), "Enter open  Esc cancel");
    }

    #[test]
    fn keys_are_bold_cyan_and_descriptions_are_not() {
        let line = hint_line(&[("Enter", "open"), ("Esc", "cancel")]);
        for s in &line.spans {
            let key = ["Enter", "Esc"].contains(&s.content.as_ref());
            assert_eq!(is_key(s), key, "{:?}", s);
            if !key {
                assert_eq!(s.style.fg, None, "{:?}", s);
                assert!(!s.style.add_modifier.contains(Modifier::BOLD), "{:?}", s);
            }
        }
        assert!(line.spans.iter().any(is_key));
    }

    #[test]
    fn empty_description_is_just_the_key() {
        let line = hint_line(&[("Esc", ""), ("q", "quit")]);
        assert_eq!(plain(&line), "Esc  q quit");
        assert!(line.spans.iter().any(|s| s.content == "Esc" && is_key(s)));
    }

    #[test]
    fn separator_variant_keeps_spacing() {
        assert_eq!(plain(&hint_line_sep(&[("Tab", "next"), ("Space", "toggle")], "   ")), "Tab next   Space toggle");
        assert_eq!(plain(&hint_line_sep(&[("n", ""), ("q", "quit")], "    ")), "n    q quit");
    }
}
