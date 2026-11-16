// Command-line front end: one subcommand per step, joined by a plan file (see plan.rs).
mod analyze;
mod cache;
mod edit;
mod preview;
mod progress;
mod prompt;
mod rescan;
mod run;
mod split;
mod time;

use audiobook_splitter_rust::cache::Cache;
use audiobook_splitter_rust::meta::{Cover, Metadata};
use audiobook_splitter_rust::pipeline::file_name;
use audiobook_splitter_rust::plan::{Plan, Settings};
use audiobook_splitter_rust::user_path;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Parser)]
#[command(
    name = "audiobook-splitter-rust",
    version,
    about = "Split audiobooks into chapters at silences, optionally matched to the book's EPUB",
    long_about = "Split audiobooks into chapters at silences, optionally matched to the book's EPUB.\n\n\
        Work in steps joined by a plan file: `analyze` writes it, `edit` (and `rescan`) change it, \
        `split` writes the audio files from it. `run` does all three in one go.",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Detect silences (and match EPUB chapters) and write a plan file
    Analyze(analyze::Args),
    /// Write the tagged audio files described by a plan file
    Split(split::Args),
    /// Review and change a plan's chapters, titles and tags (interactive)
    Edit(edit::Args),
    /// Look again for chapters the EPUB match missed
    Rescan(rescan::Args),
    /// Cut the audio after a chapter break to a file, to check it by ear
    Preview(preview::Args),
    /// Analyze, review and split in one go (prompts for anything left out)
    Run(run::Args),
    /// Show or empty the cache of silences and transcripts
    Cache(cache::Args),
    /// Start the web GUI
    Gui,
}

// The shared cache, unless `--no-cache` asked for a run without it.
pub fn open_cache(no_cache: bool) -> Cache {
    if no_cache { Cache::disabled() } else { Cache::open(Cache::default_dir()) }
}

// Shared by `analyze` and `run`.
#[derive(clap::Args)]
pub struct Detect {
    /// The book's EPUB: split only where the narration matches a chapter start
    #[arg(long)]
    pub epub: Option<PathBuf>,
    /// Shortest silence, in seconds, that can be a break (use 1-2 with an EPUB)
    #[arg(long, default_value_t = 2.0)]
    pub min_silence: f64,
    /// Level in dB below which audio counts as silence; less negative finds more
    #[arg(long, default_value_t = -40.0, allow_negative_numbers = true)]
    pub noise_db: f64,
    /// Minimum EPUB match score, 0 to 1
    #[arg(long, default_value_t = 0.4)]
    pub threshold: f64,
    /// Don't read or write the cache (silences and transcripts) for this run
    #[arg(long)]
    pub no_cache: bool,
}

impl Detect {
    pub fn settings(&self) -> Settings {
        Settings { min_silence: self.min_silence, noise_db: self.noise_db, threshold: self.threshold }
    }
}

// Shared by `split` and `run`. Anything left out keeps the plan's value.
#[derive(clap::Args)]
pub struct Tags {
    /// Album tag
    #[arg(long)]
    pub album: Option<String>,
    /// Artist tag
    #[arg(long)]
    pub artist: Option<String>,
    /// Album artist tag
    #[arg(long)]
    pub album_artist: Option<String>,
    /// Genre tag
    #[arg(long)]
    pub genre: Option<String>,
    /// Year tag
    #[arg(long)]
    pub year: Option<String>,
    /// File naming, with {n} (counts from 00), {title} and {album}
    #[arg(long)]
    pub template: Option<String>,
    /// Album artwork: `keep` (the source's own cover, default), `none`, or a JPEG/PNG path
    #[arg(long, value_name = "keep|none|PATH")]
    pub cover: Option<String>,
}

impl Tags {
    pub fn apply(&self, meta: &mut Metadata) {
        fn set(field: &mut String, value: &Option<String>) {
            if let Some(v) = value {
                *field = v.clone();
            }
        }
        set(&mut meta.album, &self.album);
        set(&mut meta.artist, &self.artist);
        set(&mut meta.album_artist, &self.album_artist);
        set(&mut meta.genre, &self.genre);
        set(&mut meta.year, &self.year);
        set(&mut meta.template, &self.template);
        match self.cover.as_deref() {
            Some("keep") => meta.cover = Cover::Keep,
            Some("none") => meta.cover = Cover::None,
            Some(path) => meta.cover = Cover::File(user_path(path)),
            None => {}
        }
    }
}

pub fn check_tools() -> Result<(), String> {
    for tool in ["ffmpeg", "ffprobe"] {
        Command::new(tool)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| {
                format!(
                    "{} was not found on your PATH. Install FFmpeg (macOS: brew install ffmpeg; Debian/Ubuntu: sudo apt install ffmpeg; Windows: winget install Gyan.FFmpeg).",
                    tool
                )
            })?;
    }
    Ok(())
}

// Loads a plan and checks its audio files are still the ones it was made from.
pub fn open_plan(path: &Path) -> Result<Plan, String> {
    let plan = Plan::load(path)?;
    plan.check_files()?;
    Ok(plan)
}

// "0:05:12", prefixed with the file name when the book has several files.
pub fn place(plan: &Plan, file: usize, at: f64) -> String {
    let when = time::clock(at);
    if plan.files.len() > 1 { format!("{} @ {}", file_name(&plan.files[file].path), when) } else { when }
}

pub fn run(mut args: Vec<String>) {
    // The old `--cli` flag is now `run`.
    if args.get(1).is_some_and(|a| a == "--cli") {
        args[1] = "run".into();
    }
    let result = match Cli::parse_from(args).cmd {
        Cmd::Analyze(a) => analyze::run(a),
        Cmd::Split(a) => split::run(a),
        Cmd::Edit(a) => edit::run(a),
        Cmd::Rescan(a) => rescan::run(a),
        Cmd::Preview(a) => preview::run(a),
        Cmd::Run(a) => run::run(a),
        Cmd::Cache(a) => cache::run(a),
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

#[cfg(test)]
mod tests {
    use super::*;
    use audiobook_splitter_rust::meta::Cover;

    fn tags(cover: Option<&str>) -> Tags {
        Tags { album: None, artist: None, album_artist: None, genre: None, year: None, template: None, cover: cover.map(String::from) }
    }

    #[test]
    fn the_cover_flag_maps_onto_the_metadata() {
        let mut meta = Metadata::default();
        tags(Some("none")).apply(&mut meta);
        assert_eq!(meta.cover, Cover::None);
        tags(Some("/art/front.png")).apply(&mut meta);
        assert_eq!(meta.cover, Cover::File("/art/front.png".into()));
        tags(Some("'/art/with space.jpg'")).apply(&mut meta);
        assert_eq!(meta.cover, Cover::File("/art/with space.jpg".into()), "quotes from pasted paths are stripped");
        tags(Some("keep")).apply(&mut meta);
        assert_eq!(meta.cover, Cover::Keep);
        tags(None).apply(&mut meta);
        assert_eq!(meta.cover, Cover::Keep, "no flag leaves the plan's choice alone");
    }
}
