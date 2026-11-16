// The screen shown while a job runs: the current stage as a gauge, and the log below it.
use crate::cli::time::clock;
use audiobook_splitter_rust::pipeline::Event;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Gauge, Paragraph};
use ratatui::Frame;
use std::time::Instant;

const KEEP_LINES: usize = 500;

pub struct Progress {
    title: String,
    stage: String,
    done: usize,
    total: usize,
    pub(super) log: Vec<String>,
    started: Instant,
}

impl Progress {
    pub fn new(title: &str) -> Progress {
        Progress { title: title.to_string(), stage: String::new(), done: 0, total: 0, log: Vec::new(), started: Instant::now() }
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Stage { name, total } => {
                self.stage = name;
                self.total = total;
                self.done = 0;
            }
            Event::Tick(done) => self.done = done,
            Event::Log(line) => {
                self.log.push(line);
                if self.log.len() > KEEP_LINES {
                    self.log.drain(..self.log.len() - KEEP_LINES);
                }
            }
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let block = Block::bordered().title(format!(" {} ", self.title));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [gauge_row, log_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
        let ratio = if self.total == 0 { 0.0 } else { (self.done as f64 / self.total as f64).clamp(0.0, 1.0) };
        let label = format!("{}  {}/{}  {}", if self.stage.is_empty() { "Starting" } else { &self.stage }, self.done, self.total, clock(self.started.elapsed().as_secs_f64()));
        f.render_widget(Gauge::default().ratio(ratio).label(label), gauge_row);
        let rows = log_area.height as usize;
        let tail: Vec<Line> = self.log.iter().skip(self.log.len().saturating_sub(rows)).map(|l| Line::raw(l.as_str())).collect();
        f.render_widget(Paragraph::new(tail), log_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn screen(p: &Progress, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| p.render(f, f.area())).unwrap();
        let buf = t.backend().buffer();
        (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn events_drive_the_gauge_and_the_log() {
        let mut p = Progress::new("Analyzing");
        p.apply(Event::Stage { name: "Detecting silences".into(), total: 10 });
        p.apply(Event::Tick(3));
        p.apply(Event::Log("first line".into()));
        let s = screen(&p, 80, 24);
        for want in ["Analyzing", "Detecting silences", "3/10", "first line"] {
            assert!(s.contains(want), "missing {:?} in\n{}", want, s);
        }
        p.apply(Event::Stage { name: "Matching chapters".into(), total: 4 });
        let s = screen(&p, 80, 24);
        assert!(s.contains("Matching chapters") && s.contains("0/4"), "a new stage resets the count:\n{}", s);
    }

    #[test]
    fn only_the_tail_of_a_long_log_is_kept_and_shown() {
        let mut p = Progress::new("Splitting");
        for i in 0..1000 {
            p.apply(Event::Log(format!("line {}", i)));
        }
        assert_eq!((p.log.len(), p.log[0].as_str()), (500, "line 500"));
        let s = screen(&p, 80, 12);
        assert!(s.contains("line 999") && !s.contains("line 990 "), "only the last rows fit: {}", s);
    }

    #[test]
    fn it_draws_at_small_sizes_and_with_no_stage_yet() {
        let p = Progress::new("Analyzing");
        for (w, h) in [(60, 15), (30, 5), (10, 3)] {
            screen(&p, w, h);
        }
    }
}
