// `run`: analyze, review and split in one go. Missing paths are prompted for.
use super::analyze::{build_plan, summary};
use super::edit::edit_plan;
use super::split::write_output;
use super::{prompt, Detect, Tags};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// Use the plain menu instead of the full-screen editor
    #[arg(long)]
    pub simple: bool,
    /// An audio file, or a folder of audio files (asked for if left out)
    pub input: Option<PathBuf>,
    /// Folder for the output files (asked for if left out)
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Where to save the plan (default: plan.json in the output folder)
    #[arg(long)]
    pub plan: Option<PathBuf>,
    /// Don't open the chapter editor; split what the analysis found
    #[arg(short, long)]
    pub yes: bool,
    #[command(flatten)]
    pub detect: Detect,
    #[command(flatten)]
    pub tags: Tags,
}

pub fn run(mut args: Args) -> Result<(), String> {
    // With no input given this is a guided session, like the old interactive mode.
    let guided = args.input.is_none();
    let input = match args.input.take() {
        Some(p) => p,
        None => prompt::ask_input_path()?,
    };
    let out = match args.out.take() {
        Some(p) => p,
        None => prompt::ask_output_dir()?,
    };
    if guided && args.detect.epub.is_none() {
        args.detect.epub = prompt::ask_optional_epub()?;
    }

    // Check where things will be written before the (possibly hours-long) analysis.
    if out.exists() && !out.is_dir() {
        return Err(format!("'{}' is not a folder.", out.display()));
    }
    std::fs::create_dir_all(&out).map_err(|e| format!("Failed to create '{}': {}", out.display(), e))?;
    let plan_path = args.plan.take().unwrap_or_else(|| out.join("plan.json"));
    if plan_path.is_dir() {
        return Err(format!("'{}' is a folder; give --plan a file name.", plan_path.display()));
    }
    if let Some(parent) = plan_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create '{}': {}", parent.display(), e))?;
    }

    let mut plan = build_plan(&input, &args.detect)?;
    args.tags.apply(&mut plan.metadata);
    plan.save(&plan_path)?;
    summary(&plan, Some(&plan_path));

    if !args.yes && !prompt::is_terminal() {
        eprintln!("Not a terminal: splitting without review (use `edit` on the plan to review it).");
    }
    if prompt::is_terminal() && !args.yes {
        if !edit_plan(&mut plan, args.simple)? {
            println!("Stopped before splitting. The plan is saved at {}; `split` it when ready.", plan_path.display());
            return Ok(());
        }
        plan.save(&plan_path)?;
    }
    write_output(&plan, &out)
}
