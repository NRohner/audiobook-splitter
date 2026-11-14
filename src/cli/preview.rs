// `preview`: cut the audio after a break to a file, to check it by ear.
use super::{open_plan, place};
use audiobook_splitter_rust::pipeline::PREVIEW_SECONDS;
use audiobook_splitter_rust::preview_clip;
use std::path::PathBuf;
use std::process::Command;

#[derive(clap::Args)]
pub struct Args {
    /// Plan file made by `analyze`
    pub plan: PathBuf,
    /// Chapter number, from the `#` column of the chapter list
    pub n: usize,
    /// Where to write the clip (default: break-N.mp3 in the current folder)
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Also play it with ffplay (needs a local audio device)
    #[arg(long)]
    pub play: bool,
}

pub fn run(args: Args) -> Result<(), String> {
    super::check_tools()?;
    let plan = open_plan(&args.plan)?;
    let b = args
        .n
        .checked_sub(1)
        .and_then(|i| plan.findings.breaks.get(i))
        .ok_or(format!("There is no chapter {} in this plan.", args.n))?;
    let mp3 = preview_clip(&plan.files[b.file].path, b.at, PREVIEW_SECONDS)?;
    let out = args.out.unwrap_or_else(|| PathBuf::from(format!("break-{}.mp3", args.n)));
    std::fs::write(&out, mp3).map_err(|e| format!("Failed to write '{}': {}", out.display(), e))?;
    println!("Wrote {} s from \"{}\" ({}) to {}.", PREVIEW_SECONDS, b.title, place(&plan, b.file, b.at), out.display());

    // ponytail: only plays on request; detecting a local audio device isn't portable.
    if args.play {
        Command::new("ffplay")
            .args(["-nodisp", "-autoexit", "-loglevel", "quiet"])
            .arg(&out)
            .status()
            .map_err(|_| "ffplay was not found on your PATH (it ships with FFmpeg). Play the file yourself, e.g. after `scp`.".to_string())?;
    }
    Ok(())
}
