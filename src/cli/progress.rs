// Draws pipeline events as indicatif bars, or as plain lines when stderr isn't a terminal
// (a log file, `ssh host cmd > out`), where bars would be noise.
use audiobook_splitter_rust::pipeline::Event;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::io::IsTerminal;

pub struct Reporter {
    mp: MultiProgress,
    bar: Option<ProgressBar>,
    tty: bool,
}

impl Reporter {
    pub fn new() -> Self {
        Reporter { mp: MultiProgress::new(), bar: None, tty: std::io::stderr().is_terminal() }
    }

    pub fn emit(&mut self, event: Event) {
        match event {
            Event::Stage { name, total } => {
                self.finish();
                if !self.tty {
                    eprintln!("{} ({} items)", name, total);
                }
                let bar = self.mp.add(ProgressBar::new(total as u64));
                bar.set_style(
                    ProgressStyle::with_template("{msg:<40} [{bar:30.cyan/blue}] {pos}/{len}  {elapsed_precise}  ETA {eta}")
                        .expect("static template")
                        .progress_chars("=> "),
                );
                bar.set_message(name);
                self.bar = Some(bar);
            }
            Event::Tick(done) => {
                if let Some(bar) = &self.bar {
                    bar.set_position(done as u64);
                }
            }
            Event::Log(line) => {
                if self.tty {
                    let _ = self.mp.println(line);
                } else {
                    eprintln!("{}", line);
                }
            }
        }
    }

    // Leaves the last bar on screen at its final position.
    pub fn finish(&mut self) {
        if let Some(bar) = self.bar.take() {
            bar.finish();
        }
    }
}
