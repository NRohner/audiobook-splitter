mod cli;
mod gui;

use audiobook_splitter_rust::gpu;

fn main() {
    if gpu::is_probe() {
        gpu::probe();
        return;
    }
    let args: Vec<String> = std::env::args().collect();
    // The GUI re-runs this binary as `--pick <kind>` to show a native file dialog.
    if let Some(i) = args.iter().position(|a| a == "--pick") {
        gui::pick(args.get(i + 1).map_or("file", String::as_str));
        return;
    }
    gpu::guard_vulkan();
    cli::run(args);
}
