mod cli;
mod gui;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // The GUI re-runs this binary as `--pick <kind>` to show a native file dialog.
    if let Some(i) = args.iter().position(|a| a == "--pick") {
        gui::pick(args.get(i + 1).map_or("file", String::as_str));
        return;
    }
    cli::run(args);
}
