// `preview`: cut the audio after a break to a file, to check it by ear.
use super::{open_plan, place};
use audiobook_splitter_rust::pipeline::PREVIEW_SECONDS;
use audiobook_splitter_rust::plan::Plan;
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

// Cuts PREVIEW_SECONDS after chapter `n` (1-based, as in the chapter list) to `out` (default
// break-N.mp3 in the current folder). Returns the file written and a "Wrote ..." line.
pub fn write_clip(plan: &Plan, n: usize, out: Option<PathBuf>) -> Result<(PathBuf, String), String> {
    let b = n
        .checked_sub(1)
        .and_then(|i| plan.findings.breaks.get(i))
        .ok_or(format!("There is no chapter {} in this plan.", n))?;
    let mp3 = preview_clip(&plan.files[b.file].path, b.at, PREVIEW_SECONDS)?;
    let out = out.unwrap_or_else(|| PathBuf::from(format!("break-{}.mp3", n)));
    std::fs::write(&out, mp3).map_err(|e| format!("Failed to write '{}': {}", out.display(), e))?;
    let line = format!("Wrote {} s from \"{}\" ({}) to {}.", PREVIEW_SECONDS, b.title, place(plan, b.file, b.at), out.display());
    Ok((out, line))
}

pub fn run(args: Args) -> Result<(), String> {
    super::check_tools()?;
    let plan = open_plan(&args.plan)?;
    let (out, line) = write_clip(&plan, args.n, args.out)?;
    println!("{}", line);

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

#[cfg(test)]
mod tests {
    use super::*;
    use audiobook_splitter_rust::meta::Metadata;
    use audiobook_splitter_rust::plan::{Findings, Settings};

    #[test]
    fn write_clip_refuses_a_chapter_that_does_not_exist() {
        let plan = Plan::new(vec![], Settings::default(), None, Findings::default(), Metadata::default());
        assert_eq!(write_clip(&plan, 0, None).unwrap_err(), "There is no chapter 0 in this plan.");
        assert_eq!(write_clip(&plan, 4, None).unwrap_err(), "There is no chapter 4 in this plan.");
    }
}
