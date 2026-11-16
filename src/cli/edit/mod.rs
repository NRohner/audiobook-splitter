// `edit`: change a plan's chapters and tags by hand. `ops` holds the plain edits on the plan
// (unit-tested, shared by both editors); `menu` is the prompt-based editor.
mod menu;
pub(crate) mod ops;

use super::{open_plan, prompt};
pub use menu::edit_plan;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// Plan file made by `analyze`; edited in place
    pub plan: PathBuf,
}

pub fn run(args: Args) -> Result<(), String> {
    prompt::require_terminal("edit")?;
    let mut plan = open_plan(&args.plan)?;
    if edit_plan(&mut plan)? {
        plan.save(&args.plan)?;
        println!("Saved {}.", args.plan.display());
    } else {
        println!("Nothing saved.");
    }
    Ok(())
}
