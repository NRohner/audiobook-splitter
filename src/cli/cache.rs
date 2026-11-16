// `cache`: look at or empty the shared cache of silences and transcripts.
use super::open_cache;
use audiobook_splitter_rust::collect_inputs;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: Sub,
}

#[derive(clap::Subcommand)]
pub enum Sub {
    /// Show where the cache is and how big it is
    Info,
    /// Delete cached silences and transcripts: everything, or only for the given audio files or folders
    Clear {
        /// Audio files or folders whose entries to delete (default: everything)
        paths: Vec<PathBuf>,
    },
}

fn human_bytes(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB"];
    let (mut value, mut unit) = (bytes as f64, 0);
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{} B", bytes) } else { format!("{:.1} {}", value, units[unit]) }
}

pub fn run(args: Args) -> Result<(), String> {
    let cache = open_cache(false);
    match args.cmd {
        Sub::Info => {
            let info = cache.info();
            match &info.dir {
                Some(dir) => {
                    println!("Cache folder: {}", dir.display());
                    println!("Entries:      {} book file(s)", info.entries);
                    println!("Size:         {}", human_bytes(info.bytes));
                }
                None => println!("Cache folder: none available (set AUDIOBOOK_CACHE_DIR to choose one)"),
            }
        }
        Sub::Clear { paths } => {
            let mut audio = Vec::new();
            for p in &paths {
                audio.extend(collect_inputs(p)?);
            }
            let removed = cache.clear(&audio)?;
            println!("Removed {} cache entr{}.", removed, if removed == 1 { "y" } else { "ies" });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
