// `split`: write the tagged audio files a plan describes.
use super::progress::Reporter;
use super::{check_tools, open_plan, Tags};
use audiobook_splitter_rust::pipeline::{prepare_split, write_tracks};
use audiobook_splitter_rust::plan::Plan;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct Args {
    /// Plan file made by `analyze`
    pub plan: PathBuf,
    /// Folder for the output files (created if needed)
    #[arg(short, long)]
    pub out: PathBuf,
    #[command(flatten)]
    pub tags: Tags,
}

pub fn run(args: Args) -> Result<(), String> {
    check_tools()?;
    let mut plan = open_plan(&args.plan)?;
    args.tags.apply(&mut plan.metadata);
    write_output(&plan, &args.out)
}

pub fn write_output(plan: &Plan, out: &Path) -> Result<(), String> {
    if out.exists() && !out.is_dir() {
        return Err(format!("'{}' is not a folder.", out.display()));
    }
    let (segments, names) = prepare_split(&plan.files, &plan.findings.to_breaks(), &plan.metadata)?;
    std::fs::create_dir_all(out).map_err(|e| format!("Failed to create '{}': {}", out.display(), e))?;

    let mut reporter = Reporter::new();
    let report = write_tracks(&segments, &names, out, &mut |e| reporter.emit(e));
    reporter.finish();
    println!("Wrote {} file(s) to {}.", report.written.len(), out.display());
    if report.failed > 0 {
        return Err(format!("{} file(s) could not be written.", report.failed));
    }
    Ok(())
}
