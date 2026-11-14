// `rescan`: look again for the chapters the match missed, with looser silence settings.
use super::analyze::summary;
use super::progress::Reporter;
use super::{check_tools, open_cache, open_plan};
use audiobook_splitter_rust::pipeline::{chapter_title, open_epub, rescan_one, LazyTranscriber};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// Plan file made by `analyze --epub`; updated in place
    pub plan: PathBuf,
    /// Only this missing chapter (its number in the "No break found" list); repeatable
    #[arg(long = "chapter", value_name = "N")]
    pub chapters: Vec<usize>,
    /// Don't read or write the transcript cache for this run
    #[arg(long)]
    pub no_cache: bool,
}

pub fn run(args: Args) -> Result<(), String> {
    check_tools()?;
    let mut plan = open_plan(&args.plan)?;
    let epub_path = plan.epub.clone().ok_or("This plan was made without an EPUB, so there is nothing to re-scan against.")?;
    let (epub, _) = open_epub(&epub_path)?;

    let missing: Vec<usize> = plan.findings.missing.iter().map(|m| m.chapter).collect();
    let mut wanted = if args.chapters.is_empty() {
        missing.clone()
    } else {
        let mut chosen = Vec::new();
        for n in &args.chapters {
            let ch = n.checked_sub(1).filter(|c| missing.contains(c)).ok_or(format!("Chapter {} isn't missing from this plan.", n))?;
            chosen.push(ch);
        }
        chosen
    };
    if wanted.is_empty() {
        println!("Nothing is missing.");
        return Ok(());
    }
    wanted.sort();
    wanted.dedup();

    let cache = open_cache(args.no_cache);
    let mut transcriber = LazyTranscriber::new(&epub.model);
    let mut reporter = Reporter::new();
    let mut outcome = Ok(());
    for ch in wanted {
        let found = plan.findings.found.clone();
        match rescan_one(ch, &plan.files, &found, &plan.settings, &epub, &mut transcriber, &cache, &mut |e| reporter.emit(e)) {
            Ok(r) => {
                plan.findings.apply_rescan(ch, chapter_title(&epub.chapters, ch), r.found, r.silences);
                // Save as we go: an interrupted run loses at most the chapter in flight.
                plan.sort_breaks();
                plan.save(&args.plan)?;
            }
            Err(e) => {
                outcome = Err(e);
                break;
            }
        }
    }
    reporter.finish();
    // Keep what was found even if a later chapter failed.
    plan.sort_breaks();
    plan.save(&args.plan)?;
    outcome?;
    summary(&plan, Some(&args.plan));
    Ok(())
}
