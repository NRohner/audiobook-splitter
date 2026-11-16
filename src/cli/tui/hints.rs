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

#[cfg(test)]
pub fn hint_line(pairs: &[(&str, &str)]) -> Line<'static> {
    Line::from(hint_spans(pairs))
}

pub fn hint_line_sep(pairs: &[(&str, &str)], sep: &str) -> Line<'static> {
    Line::from(hint_spans_sep(pairs, sep))
}

// Greedy line filling between commands: a command (key, space, description) is never split across lines.
// A command wider than `width` stands alone on its line (the renderer clips it).
pub fn wrap_hints(pairs: &[(&str, &str)], sep: &str, width: usize) -> Vec<Line<'static>> {
    let sep_w = sep.chars().count();
    let mut lines = Vec::new();
    let (mut start, mut used) = (0, 0);
    for (i, (key, desc)) in pairs.iter().enumerate() {
        let w = key.chars().count() + if desc.is_empty() { 0 } else { 1 + desc.chars().count() };
        if i > start && used + sep_w + w > width {
            lines.push(hint_line_sep(&pairs[start..i], sep));
            (start, used) = (i, w);
        } else {
            used += if i > start { sep_w + w } else { w };
        }
    }
    if start < pairs.len() {
        lines.push(hint_line_sep(&pairs[start..], sep));
    }
    lines
}

// Word-wraps `text` to `width` columns; a word longer than a line is split hard.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        while word.chars().count() > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let head: String = word.chars().take(width).collect();
            word = word.chars().skip(width).collect();
            lines.push(head);
        }
        let used = cur.chars().count();
        if used > 0 && used + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&word);
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
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

    const LIST: &[(&str, &str)] = &[("j/k", "move"), ("r", "rename"), ("[ ]", "move break"), ("d", "remove"), ("a", "add"), ("n", "silence"), ("m", "missing"), ("t", "tags"), ("g", "jump")];

    fn cmd(p: &(&str, &str)) -> String {
        if p.1.is_empty() { p.0.to_string() } else { format!("{} {}", p.0, p.1) }
    }

    #[test]
    fn wrapping_keeps_whole_commands_in_order_within_the_width() {
        let total = plain(&hint_line(LIST)).chars().count();
        assert!(total > 50, "{}", total);
        for width in [20usize, 30, 40, 80] {
            let lines = wrap_hints(LIST, "  ", width);
            assert!(!lines.is_empty());
            let widest = LIST.iter().map(|p| cmd(p).chars().count()).max().unwrap();
            let mut seen = Vec::new();
            for l in &lines {
                let text = plain(l);
                assert!(text.chars().count() <= width.max(widest), "{:?} at {}", text, width);
                for c in text.split("  ") {
                    seen.push(c.to_string());
                }
            }
            let want: Vec<String> = LIST.iter().map(cmd).collect();
            assert_eq!(seen, want, "width {}", width);
            if width >= total {
                assert_eq!(lines.len(), 1);
            } else {
                assert!(lines.len() > 1, "width {}", width);
            }
        }
    }

    #[test]
    fn a_line_text_matches_hint_line_sep_for_its_subset() {
        let lines = wrap_hints(LIST, "   ", 30);
        let mut at = 0;
        for l in &lines {
            let n = plain(l).split("   ").count();
            assert_eq!(plain(l), plain(&hint_line_sep(&LIST[at..at + n], "   ")));
            at += n;
        }
        assert_eq!(at, LIST.len());
    }

    #[test]
    fn an_oversize_command_sits_alone() {
        let pairs = [("a", "b"), ("Enter", "a very long description indeed"), ("c", "d")];
        let lines = wrap_hints(&pairs, "  ", 12);
        let texts: Vec<String> = lines.iter().map(plain).collect();
        assert_eq!(texts, vec!["a b", "Enter a very long description indeed", "c d"]);
    }

    #[test]
    fn wrapped_lines_style_keys_only_and_handle_empty_descriptions() {
        let pairs = [("Esc", ""), ("q", "quit"), ("?", "help")];
        let lines = wrap_hints(&pairs, "  ", 9);
        let texts: Vec<String> = lines.iter().map(plain).collect();
        assert_eq!(texts, vec!["Esc", "q quit", "? help"]);
        for l in &lines {
            for s in &l.spans {
                let key = ["Esc", "q", "?"].contains(&s.content.as_ref());
                assert_eq!(is_key(s), key, "{:?}", s);
            }
        }
    }
}

#[cfg(test)]
mod wrap_tests {
    use super::wrap_text;

    #[test]
    fn wraps_on_words_and_splits_overlong_ones() {
        assert_eq!(wrap_text("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrap_text("abcdefg", 3), ["abc", "def", "g"]);
        assert_eq!(wrap_text("", 5), [""]);
    }
}
