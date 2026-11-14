// `analyze`: detect silences, optionally match the EPUB, write the plan.
use super::progress::Reporter;
use super::{check_tools, place, Detect};
use audiobook_splitter_rust::collect_inputs;
use audiobook_splitter_rust::pipeline::{analyze, default_metadata, open_epub};
use audiobook_splitter_rust::plan::Plan;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct Args {
    /// An audio file, or a folder of audio files (treated as one book, in file-name order)
    pub input: PathBuf,
    /// Where to write the plan
    #[arg(short, long, default_value = "plan.json")]
    pub out: PathBuf,
    #[command(flatten)]
    pub detect: Detect,
}

pub fn run(args: Args) -> Result<(), String> {
    let plan = build_plan(&args.input, &args.detect)?;
    plan.save(&args.out)?;
    summary(&plan, Some(&args.out));
    println!("\nPlan written to {}.", args.out.display());
    Ok(())
}

// Checks everything cheap first, then runs the analysis with progress bars.
pub fn build_plan(input: &Path, detect: &Detect) -> Result<Plan, String> {
    check_tools()?;
    let settings = detect.settings();
    settings.validate()?;
    let inputs = collect_inputs(input)?;
    if inputs.is_empty() {
        return Err(format!("No supported audio files found in '{}'.", input.display()));
    }
    let (epub, meta) = match &detect.epub {
        Some(path) => {
            let (epub, info) = open_epub(path)?;
            (Some(epub), default_metadata(Some(info)))
        }
        None => (None, default_metadata(None)),
    };

    let mut reporter = Reporter::new();
    let result = analyze(&inputs, &settings, epub.as_ref(), &mut |e| reporter.emit(e));
    reporter.finish();
    let analysis = result?;
    if analysis.files.is_empty() {
        return Err("None of the audio files could be analyzed.".into());
    }
    Ok(Plan::new(analysis.files, settings, detect.epub.clone(), analysis.findings, meta))
}

// The chapter list, and what to run next.
pub fn summary(plan: &Plan, plan_path: Option<&Path>) {
    let f = &plan.findings;
    println!("\n{:>3}  {:<28} {:<40} {}", "#", "Where", "Title", "Match");
    for (i, b) in f.breaks.iter().enumerate() {
        let score = b.score.map_or(String::new(), |s| format!("{:.2}", s));
        println!("{:>3}  {:<28} {:<40} {}", i + 1, place(plan, b.file, b.at), b.title, score);
    }
    println!("\n{} chapter(s).", f.breaks.len());
    if !f.missing.is_empty() {
        let names: Vec<String> = f.missing.iter().map(|m| format!("{}. {}", m.chapter + 1, m.title)).collect();
        println!("No break found for: {}", names.join(", "));
        if let Some(p) = plan_path {
            println!("Try `rescan {}`, or a smaller --min-silence or --threshold.", p.display());
        }
    }
}
