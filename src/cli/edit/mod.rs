// `edit`: change a plan's chapters and tags by hand. `ops` holds the plain edits on the plan
// (unit-tested, shared by both editors); `menu` is the prompt-based editor.
mod menu;
pub(crate) mod ops;

use super::{open_plan, prompt};
use super::tui::{self, terminal::TerminalGuard};
use audiobook_splitter_rust::plan::Plan;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// Plan file made by `analyze`; edited in place
    pub plan: PathBuf,
    /// Use the plain menu instead of the full-screen editor
    #[arg(long)]
    pub simple: bool,
}

// Runs the editor on `plan`: full screen when the terminal suits it, else the prompt menu.
// Returns whether to save.
pub fn edit_plan(plan: &mut Plan, simple: bool) -> Result<bool, String> {
    if tui::terminal::suitable(simple) {
        match TerminalGuard::new() {
            Ok(guard) => return tui::edit(plan, guard),
            Err(e) => println!("Full-screen editor unavailable ({}); using the plain menu.", e),
        }
    }
    menu::edit_plan(plan)
}

pub fn run(args: Args) -> Result<(), String> {
    prompt::require_terminal("edit")?;
    let mut plan = open_plan(&args.plan)?;
    if edit_plan(&mut plan, args.simple)? {
        plan.save(&args.plan)?;
        println!("Saved {}.", args.plan.display());
    } else {
        println!("Nothing saved.");
    }
    Ok(())
}
