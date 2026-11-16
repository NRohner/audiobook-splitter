// The tags, file naming and cover art form, opened from the editor with `t`.
use super::input::Input;
use crate::cli::edit::ops;
use audiobook_splitter_rust::cover::find_embedded;
use audiobook_splitter_rust::meta::Cover;
use audiobook_splitter_rust::pipeline::file_name;
use audiobook_splitter_rust::plan::Plan;
use audiobook_splitter_rust::user_path;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

const LABELS: [&str; 6] = ["Album", "Artist", "Album artist", "Genre", "Year", "File naming"];
const COVER_ROW: usize = 6;
const IMAGE_ROW: usize = 7;

pub enum FormOutcome {
    Continue,
    Save,
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CoverChoice {
    Keep,
    None,
    Image,
}

pub struct TagsForm {
    inputs: Vec<Input>, // album, artist, album artist, genre, year, file naming
    cover: CoverChoice,
    image: Input,
    embedded: Option<String>, // the file `Keep` would take the cover from
    focus: usize,             // 0..=5 the text fields, COVER_ROW, IMAGE_ROW (only for an image)
    pub error: String,
}

impl TagsForm {
    pub fn from_plan(plan: &Plan) -> TagsForm {
        let m = &plan.metadata;
        let (cover, image) = match &m.cover {
            Cover::Keep => (CoverChoice::Keep, String::new()),
            Cover::None => (CoverChoice::None, String::new()),
            Cover::File(path) => (CoverChoice::Image, path.display().to_string()),
        };
        TagsForm {
            inputs: [&m.album, &m.artist, &m.album_artist, &m.genre, &m.year, &m.template].iter().map(|v| Input::new(v)).collect(),
            cover,
            image: Input::new(&image),
            embedded: find_embedded(&plan.files).map(|(i, _)| file_name(&plan.files[i].path)),
            focus: 0,
            error: String::new(),
        }
    }

    fn rows(&self) -> usize {
        if self.cover == CoverChoice::Image { IMAGE_ROW + 1 } else { COVER_ROW + 1 }
    }

    fn cycle_cover(&mut self, step: i32) {
        let all = [CoverChoice::Keep, CoverChoice::None, CoverChoice::Image];
        let i = all.iter().position(|c| *c == self.cover).unwrap_or(0) as i32;
        self.cover = all[(i + step).rem_euclid(3) as usize];
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FormOutcome {
        self.error.clear();
        let rows = self.rows();
        match key.code {
            KeyCode::Esc => return FormOutcome::Cancel,
            KeyCode::Enter => return FormOutcome::Save,
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % rows,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + rows - 1) % rows,
            _ if self.focus == COVER_ROW => match key.code {
                KeyCode::Left => self.cycle_cover(-1),
                KeyCode::Right | KeyCode::Char(' ') => self.cycle_cover(1),
                _ => {}
            },
            _ if self.focus == IMAGE_ROW => {
                // Deliberate guard: Option/Alt chords that some terminals send as ALT+char must not be typed into the path.
                if !key.modifiers.contains(KeyModifiers::ALT) {
                    self.image.handle_key(key);
                }
            }
            _ => {
                self.inputs[self.focus].handle_key(key);
            }
        }
        FormOutcome::Continue
    }

    // Writes the form into the plan's metadata. Refuses (changing nothing) an empty naming
    // convention, or an image path that is empty or not a readable JPEG/PNG.
    pub fn apply(&self, plan: &mut Plan) -> Result<(), String> {
        let v: Vec<String> = self.inputs.iter().map(Input::value).collect();
        if v[5].trim().is_empty() {
            return Err("Enter a track naming convention, such as {n} - {title}.".into());
        }
        let cover = match self.cover {
            CoverChoice::Keep => Cover::Keep,
            CoverChoice::None => Cover::None,
            CoverChoice::Image => {
                if self.image.value().trim().is_empty() {
                    return Err("Enter the path of a JPEG or PNG image.".into());
                }
                Cover::File(user_path(&self.image.value()))
            }
        };
        ops::set_cover(plan, cover)?;
        let m = &mut plan.metadata;
        (m.album, m.artist, m.album_artist, m.genre, m.year, m.template) =
            (v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone(), v[4].clone(), v[5].clone());
        Ok(())
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let rows = self.rows() as u16;
        let rect = super::editor::centered(area, 76, rows + 6);
        f.render_widget(Clear, rect);
        let block = Block::bordered().title(" Tags, file naming and cover art (Tab next, Enter save, Esc cancel) ");
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let label_w = 14;
        let field_w = (inner.width as usize).saturating_sub(label_w + 1);
        let hot = Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED);
        let mut lines = Vec::new();
        let mut cursor = None;
        for (row, (label, input)) in LABELS.iter().zip(&self.inputs).enumerate() {
            let (text, col) = input.view(field_w);
            let style = if self.focus == row { hot } else { Style::new().add_modifier(Modifier::BOLD) };
            lines.push(Line::from(vec![Span::styled(format!("{:<w$}", label, w = label_w), style), Span::raw(text)]));
            if self.focus == row {
                cursor = Some((inner.x + (label_w + col) as u16, inner.y + row as u16));
            }
        }
        let mark = |c: CoverChoice| if self.cover == c { "(x)" } else { "( )" };
        let keep = match &self.embedded {
            Some(name) => format!("Keep the cover embedded in {}", name),
            None => "Keep the embedded cover (none found)".to_string(),
        };
        let style = if self.focus == COVER_ROW { hot } else { Style::new().add_modifier(Modifier::BOLD) };
        lines.push(Line::from(vec![Span::styled(format!("{:<w$}", "Cover art", w = label_w), style), Span::raw(format!("{} {}", mark(CoverChoice::Keep), keep))]));
        lines.push(Line::raw(format!("{:<w$}{} No cover art", "", mark(CoverChoice::None), w = label_w)));
        lines.push(Line::raw(format!("{:<w$}{} Use an image (JPEG or PNG)", "", mark(CoverChoice::Image), w = label_w)));
        if self.cover == CoverChoice::Image {
            let (text, col) = self.image.view(field_w);
            let style = if self.focus == IMAGE_ROW { hot } else { Style::new().add_modifier(Modifier::BOLD) };
            lines.push(Line::from(vec![Span::styled(format!("{:<w$}", "Image file", w = label_w), style), Span::raw(text)]));
            if self.focus == IMAGE_ROW {
                cursor = Some((inner.x + (label_w + col) as u16, inner.y + lines.len() as u16 - 1));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(self.error.clone(), Style::new().fg(ratatui::style::Color::Yellow)));
        f.render_widget(Paragraph::new(lines), inner);
        if let Some(pos) = cursor {
            f.set_cursor_position(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::edit::ops::sample_plan;
    use audiobook_splitter_rust::meta::Cover;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(form: &mut TagsForm, text: &str) {
        for c in text.chars() {
            form.handle_key(k(KeyCode::Char(c)));
        }
    }

    fn png(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("abs-tags-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]).unwrap();
        path
    }

    fn plan() -> Plan {
        let mut p = sample_plan();
        p.metadata.album = "Moby Dick".into();
        p.metadata.template = "{n} - {title}".into();
        p
    }

    #[test]
    fn the_form_starts_from_the_plans_metadata() {
        let form = TagsForm::from_plan(&plan());
        assert_eq!(form.inputs[0].value(), "Moby Dick");
        assert_eq!(form.inputs[5].value(), "{n} - {title}");
        assert_eq!(form.cover, CoverChoice::Keep);
        let mut p = plan();
        p.metadata.cover = Cover::File("/art/front.png".into());
        let form = TagsForm::from_plan(&p);
        assert_eq!((form.cover, form.image.value().as_str()), (CoverChoice::Image, "/art/front.png"));
    }

    #[test]
    fn typing_and_tabbing_edit_the_fields_and_enter_saves_them() {
        let mut p = plan();
        let mut form = TagsForm::from_plan(&p);
        form.handle_key(k(KeyCode::Tab)); // artist
        typed(&mut form, "Melville");
        form.handle_key(k(KeyCode::Tab)); // album artist
        form.handle_key(k(KeyCode::Tab)); // genre
        typed(&mut form, "Audiobook");
        assert!(matches!(form.handle_key(k(KeyCode::Enter)), FormOutcome::Save));
        form.apply(&mut p).unwrap();
        assert_eq!((p.metadata.artist.as_str(), p.metadata.genre.as_str(), p.metadata.album.as_str()), ("Melville", "Audiobook", "Moby Dick"));
    }

    #[test]
    fn esc_cancels_and_shift_tab_goes_back() {
        let mut form = TagsForm::from_plan(&plan());
        form.handle_key(k(KeyCode::BackTab));
        assert_eq!(form.focus, 6, "wraps to the cover row, as the image row is hidden");
        assert!(matches!(form.handle_key(k(KeyCode::Esc)), FormOutcome::Cancel));
    }

    #[test]
    fn an_empty_naming_template_is_refused_and_nothing_changes() {
        let mut p = plan();
        let mut form = TagsForm::from_plan(&p);
        for _ in 0..5 {
            form.handle_key(k(KeyCode::Tab));
        }
        form.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        typed(&mut form, "  ");
        let before = p.metadata.clone();
        assert!(form.apply(&mut p).unwrap_err().contains("naming convention"));
        assert_eq!(p.metadata, before);
    }

    #[test]
    fn the_cover_choice_cycles_and_the_image_row_appears_only_for_an_image() {
        let mut p = plan();
        let mut form = TagsForm::from_plan(&p);
        for _ in 0..6 {
            form.handle_key(k(KeyCode::Tab));
        }
        assert_eq!(form.focus, 6);
        form.handle_key(k(KeyCode::Right));
        assert_eq!(form.cover, CoverChoice::None);
        form.apply(&mut p).unwrap();
        assert_eq!(p.metadata.cover, Cover::None);
        form.handle_key(k(KeyCode::Right));
        assert_eq!(form.cover, CoverChoice::Image);
        form.handle_key(k(KeyCode::Tab));
        assert_eq!(form.focus, 7, "the image path is now reachable");
        let image = png("front.png");
        typed(&mut form, image.to_str().unwrap());
        form.apply(&mut p).unwrap();
        assert_eq!(p.metadata.cover, Cover::File(image));
        form.handle_key(k(KeyCode::BackTab));
        form.handle_key(k(KeyCode::Left));
        form.handle_key(k(KeyCode::Left));
        assert_eq!(form.cover, CoverChoice::Keep);
        form.handle_key(k(KeyCode::Tab));
        assert_eq!(form.focus, 0, "from the cover row the next field wraps to the first");
    }

    #[test]
    fn a_bad_image_is_refused_and_the_old_choice_stays() {
        let mut p = plan();
        let mut form = TagsForm::from_plan(&p);
        form.cover = CoverChoice::Image;
        assert!(form.apply(&mut p).unwrap_err().contains("path of a JPEG or PNG"));
        typed_into_image(&mut form, "/definitely/not/here.png");
        assert!(form.apply(&mut p).is_err());
        let text = std::env::temp_dir().join(format!("abs-tags-{}-text.txt", std::process::id()));
        std::fs::write(&text, "hello").unwrap();
        typed_into_image(&mut form, text.to_str().unwrap());
        assert!(form.apply(&mut p).unwrap_err().contains("not a JPEG or PNG"));
        assert_eq!(p.metadata.cover, Cover::Keep);
    }

    fn typed_into_image(form: &mut TagsForm, text: &str) {
        form.image = Input::new(text);
    }

    #[test]
    fn it_draws_its_labels_and_the_embedded_cover_note() {
        let form = TagsForm::from_plan(&plan());
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        t.draw(|f| form.render(f, f.area())).unwrap();
        let buf = t.backend().buffer();
        let screen: String = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        for want in ["Album", "Moby Dick", "Album artist", "Genre", "Year", "File naming", "Cover art", "none found"] {
            assert!(screen.contains(want), "missing {:?} in\n{}", want, screen);
        }
        let mut small = Terminal::new(TestBackend::new(60, 15)).unwrap();
        small.draw(|f| form.render(f, f.area())).unwrap();
    }

    #[test]
    fn a_refused_save_shows_its_error_at_every_size() {
        for cover in [CoverChoice::Keep, CoverChoice::Image] {
            for (w, h) in [(80, 24), (60, 15)] {
                let mut form = TagsForm::from_plan(&plan());
                form.cover = cover;
                form.error = "Enter a track naming convention, such as {n} - {title}.".into();
                let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
                t.draw(|f| form.render(f, f.area())).unwrap();
                let buf = t.backend().buffer();
                let screen: String = (0..buf.area.height)
                    .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(screen.contains("Enter a track naming convention"), "{:?} {}x{} lost the error in\n{}", cover, w, h, screen);
            }
        }
    }
}
