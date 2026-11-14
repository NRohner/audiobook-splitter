// `run`: analyze, review and split in one go. Missing paths are prompted for.
use super::analyze::{build_plan, summary};
use super::edit::edit_plan;
use super::split::write_output;
use super::{prompt, Detect, Tags};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
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

    let mut plan = build_plan(&input, &args.detect)?;
    args.tags.apply(&mut plan.metadata);
    let plan_path = args.plan.unwrap_or_else(|| out.join("plan.json"));
    plan.save(&plan_path)?;
    summary(&plan, Some(&plan_path));

    if prompt::is_terminal() && !args.yes {
        if !edit_plan(&mut plan)? {
            println!("Stopped before splitting. The plan is saved at {}; `split` it when ready.", plan_path.display());
            return Ok(());
        }
        plan.save(&plan_path)?;
    }
    write_output(&plan, &out)
}
