// Command-line front end: one subcommand per step, joined by a plan file (see plan.rs).
mod progress;
mod prompt;
mod time;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "audiobook-splitter-rust",
    version,
    about = "Split audiobooks into chapters at silences, optionally matched to the book's EPUB",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the web GUI
    Gui,
}

pub fn run(mut args: Vec<String>) {
    // The old `--cli` flag is now `run`.
    if args.get(1).is_some_and(|a| a == "--cli") {
        args[1] = "run".into();
    }
    let result: Result<(), String> = match Cli::parse_from(args).cmd {
        Cmd::Gui => {
            crate::gui::run();
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
