mod gui;

use audiobook_splitter_rust::book::{book_info, load_chapters};
use audiobook_splitter_rust::chapters::{candidates, chapter_breaks, pick_breaks, plan_from_breaks, transcribe_candidates, AudioFile, Segment, SplitJob};
use audiobook_splitter_rust::meta::{tracks, Metadata, DEFAULT_TEMPLATE};
use audiobook_splitter_rust::transcribe::{find_model, Transcriber};
use audiobook_splitter_rust::{collect_inputs, detect_silences, split_file, split_points, user_path, write_segment};
use std::io::{self, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--pick") {
        gui::pick(args.get(i + 1).map_or("file", String::as_str));
    } else if args.iter().any(|a| a == "--cli") {
        run_cli();
    } else {
        gui::run();
    }
}

// Prints `message` and returns the trimmed line the user typed.
fn prompt(message: &str) -> String {
    print!("{}", message);
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().to_string()
}

fn run_cli() {
    println!("Welcome to the Audio Splitter!");
    println!("--------------------------------");
    println!("Note: This application requires FFmpeg and FFprobe to be installed");
    println!("      and accessible in your system's PATH for audio processing.");
    println!("--------------------------------");

    loop {
        process_selection();
        if !prompt("\nDo you want to process another file or folder? (y/n): ").eq_ignore_ascii_case("y") {
            break;
        }
    }
    println!("\nThank you for using the Audio Splitter! Goodbye.");
}

fn process_selection() {
    let input_paths = loop {
        let choice = prompt("Do you want to process a (s)ingle file or a (f)older of files? (s/f): ").to_lowercase();
        let path = match choice.as_str() {
            "s" => user_path(&prompt("Enter the path to the audio file (e.g., audio.mp3 or C:\\path\\to\\audio.wav): ")),
            "f" => user_path(&prompt("Enter the path to the folder containing audio files: ")),
            _ => {
                println!("Invalid choice. Please enter 's' or 'f'.");
                continue;
            }
        };
        match collect_inputs(&path) {
            Ok(paths) if paths.is_empty() => println!("No supported audio files found in the specified folder. Please try again."),
            Ok(paths) => {
                println!("Status: Found {} audio file(s).", paths.len());
                break paths;
            }
            Err(e) => println!("Error: {} Please try again.", e),
        }
    };

    let output_dir = loop {
        let dir = user_path(&prompt("Enter the base output directory (e.g., output_splits or C:\\MyAudioSplits): "));
        if dir.is_dir() {
            break dir;
        } else if dir.exists() {
            println!("Error: The provided path is not a directory. Please enter a valid directory path.");
        } else if prompt(&format!("Output directory '{}' does not exist. Create it? (y/n): ", dir.display())).eq_ignore_ascii_case("y") {
            match std::fs::create_dir_all(&dir) {
                Ok(_) => break dir,
                Err(e) => eprintln!("Failed to create directory '{}': {}", dir.display(), e),
            }
        } else {
            println!("Cannot proceed without a valid output directory.");
        }
    };

    // Optional EPUB: splits only where the transcript matches a chapter start.
    let epub = loop {
        let path = user_path(&prompt("Optional: path to the book's EPUB to match chapters with AI (Enter to skip): "));
        if path.as_os_str().is_empty() {
            break None;
        }
        match load_chapters(&path).and_then(|ch| Ok((ch, book_info(&path)?, find_model()?))) {
            Ok((chapters, (album, artist, year), model)) => {
                println!("Status: EPUB has {} chapters.", chapters.len());
                let meta = Metadata { album, artist, year, genre: "Audiobook".into(), template: DEFAULT_TEMPLATE.into(), ..Default::default() };
                break Some((chapters, model, meta));
            }
            Err(e) => println!("Error: {} Please try again.", e),
        }
    };

    // Analysis loop: re-run with new settings until the user is happy.
    let plan = loop {
        let min_silence = loop {
            match prompt("Enter the minimum silence length in seconds (e.g., 2.0): ").parse::<f64>() {
                Ok(t) if t > 0.0 => break t,
                _ => println!("Error: Invalid threshold. Please enter a positive number."),
            }
        };
        let noise_db = loop {
            match prompt("Enter the noise threshold in dB (e.g., -40.0). Suggestion: -40.0dB. Less negative values detect more silence: ").parse::<f64>() {
                Ok(n) => break n,
                _ => println!("Error: Invalid noise threshold. Please enter a number (e.g., -40.0)."),
            }
        };

        println!("\nStatus: Detecting silences in {} file(s) with threshold {:.2}s and noise {}dB...", input_paths.len(), min_silence, noise_db);
        println!("(This might take a while for long audio files)");

        let mut files: Vec<AudioFile> = Vec::new();
        for path in &input_paths {
            print!("  Analyzing '{}'...", path.display());
            io::stdout().flush().unwrap();
            match detect_silences(path, min_silence, noise_db) {
                Ok((silences, duration)) => {
                    println!(" Identified {} segments.", split_points(&silences, min_silence, duration).len());
                    files.push(AudioFile { path: path.clone(), silences, duration });
                }
                Err(e) => eprintln!("\n  Error analyzing '{}': {}\n  (This file will be skipped.)", path.display(), e),
            }
        }

        // Per-file splits without an EPUB, whole chapters with one; the other stays empty.
        let plan: (Vec<SplitJob>, Vec<Segment>) = match &epub {
            None => (
                files
                    .iter()
                    .map(|f| SplitJob { path: f.path.clone(), points: split_points(&f.silences, min_silence, f.duration) })
                    .filter(|job| !job.points.is_empty())
                    .collect(),
                Vec::new(),
            ),
            Some((chapters, model, _)) => {
                let cands = candidates(&files, min_silence);
                println!("\nStatus: Transcribing around {} candidate breaks...", cands.len());
                let transcripts = Transcriber::load(model).and_then(|t| {
                    transcribe_candidates(&files, &cands, &t, |done, total| {
                        print!("\r  {}/{}", done, total);
                        io::stdout().flush().unwrap();
                    })
                });
                println!();
                let transcripts = match transcripts {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        return;
                    }
                };
                let m = pick_breaks(&cands, &transcripts, chapters, 0.4);
                let title = |ch: usize| chapters[ch].title.clone().unwrap_or_else(|| format!("Section {}", ch + 1));
                for b in &m.breaks {
                    println!("  {} @ {:.1}s -> {} (score {:.2})", files[b.file].path.display(), b.at, title(b.chapter), b.score);
                }
                if !m.missing.is_empty() {
                    let names: Vec<String> = m.missing.iter().map(|&ch| title(ch)).collect();
                    println!("  No break found for: {}", names.join(", "));
                }
                println!("  Matched {} of {} chapters.", m.breaks.len(), chapters.len());
                if m.breaks.is_empty() {
                    (Vec::new(), Vec::new())
                } else {
                    (Vec::new(), plan_from_breaks(&files, &chapter_breaks(&m, chapters)).expect("matched breaks are valid"))
                }
            }
        };

        let total_segments: usize = plan.0.iter().map(|job| job.points.len()).sum::<usize>() + plan.1.len();
        if total_segments == 0 {
            println!("\nNo silences detected longer than the specified threshold across all files with current settings.");
            if prompt("Do you want to re-analyze with different settings? (y/n): ").eq_ignore_ascii_case("y") {
                continue;
            }
            println!("No splitting performed for the current selection.");
            return;
        }

        println!("\nSummary: Identified {} total audio segments across {} of {} file(s).", total_segments, files.len(), input_paths.len());
        match prompt("Do you want to (r)e-analyze with different settings or (p)roceed to split all files? (r/p): ").to_lowercase().as_str() {
            "p" => break plan,
            "r" => continue,
            _ => println!("Invalid choice. Re-analyzing by default..."),
        }
    };

    let (jobs, segments) = plan;
    for SplitJob { path, points } in &jobs {
        println!("\n--- Splitting: {} ---", path.display());
        let result = split_file(path, &output_dir, points, |part, total, out| {
            println!("  Status: Writing part {}/{} to '{}'...", part, total, out.display());
        });
        match result {
            Ok(n) => println!("Successfully wrote {} file(s) for {}.", n, path.display()),
            Err(e) => eprintln!("An error occurred during splitting {}: {}", path.display(), e),
        }
    }

    // With an EPUB every chapter is a numbered, tagged track of the book.
    if let Some((_, _, meta)) = &epub {
        let tracks = tracks(&segments, meta).expect("default template is valid");
        for (i, (segment, track)) in segments.iter().zip(&tracks).enumerate() {
            println!("  Status: Writing chapter {}/{} to '{}'...", i + 1, segments.len(), track.name);
            if let Err(e) = write_segment(&segment.pieces, &output_dir, track) {
                eprintln!("An error occurred: {}", e);
            }
        }
    }
}
