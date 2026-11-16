// The tags, naming and cover form (stub; the real form arrives with its own task).
use audiobook_splitter_rust::plan::Plan;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

pub enum FormOutcome {
    Continue,
    Save,
    Cancel,
}

pub struct TagsForm {
    pub error: String,
}

impl TagsForm {
    pub fn from_plan(_plan: &Plan) -> TagsForm {
        TagsForm { error: String::new() }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FormOutcome {
        if key.code == KeyCode::Esc { FormOutcome::Cancel } else { FormOutcome::Continue }
    }

    pub fn apply(&self, _plan: &mut Plan) -> Result<(), String> {
        Ok(())
    }
}
