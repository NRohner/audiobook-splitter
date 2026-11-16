// Long work (analysis, splitting) on a worker thread, reporting through the same
// `pipeline::Event`s the CLI and GUI use, so the screen never blocks.
use audiobook_splitter_rust::pipeline::Event;
use std::sync::mpsc::{self, Receiver};
use std::thread;

pub enum Msg<T> {
    Event(Event),
    Done(Result<T, String>),
}

pub struct Job<T> {
    rx: Receiver<Msg<T>>,
}

// The message a panic carried, when it was a string.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_string())
}

impl<T: Send + 'static> Job<T> {
    pub fn spawn(work: impl FnOnce(&mut dyn FnMut(Event)) -> Result<T, String> + Send + 'static) -> Job<T> {
        let (tx, rx) = mpsc::channel();
        let fail_tx = tx.clone();
        let spawned = thread::Builder::new().name("tui-job".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                work(&mut |e| {
                    let _ = tx.send(Msg::Event(e));
                })
            }))
            .unwrap_or_else(|payload| Err(format!("The background job crashed: {}", panic_text(payload.as_ref()))));
            let _ = tx.send(Msg::Done(result));
        });
        if let Err(e) = spawned {
            let _ = fail_tx.send(Msg::Done(Err(format!("Couldn't start the background job: {}", e))));
        }
        Job { rx }
    }
}

impl<T> Job<T> {
    #[cfg(test)]
    pub fn from_receiver(rx: Receiver<Msg<T>>) -> Job<T> {
        Job { rx }
    }

    pub fn try_next(&self) -> Option<Msg<T>> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(job: &Job<u32>) -> (Vec<String>, Result<u32, String>) {
        let mut logs = Vec::new();
        for _ in 0..500 {
            while let Some(msg) = job.try_next() {
                match msg {
                    Msg::Event(Event::Log(l)) => logs.push(l),
                    Msg::Event(_) => {}
                    Msg::Done(r) => return (logs, r),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the job never finished");
    }

    #[test]
    fn a_job_streams_events_then_its_result() {
        let job = Job::spawn(|emit| {
            emit(Event::Stage { name: "Work".into(), total: 2 });
            emit(Event::Log("one".into()));
            emit(Event::Log("two".into()));
            Ok(7)
        });
        assert_eq!(drain(&job), (vec!["one".to_string(), "two".to_string()], Ok(7)));
    }

    #[test]
    fn a_failing_job_reports_its_error() {
        let job: Job<u32> = Job::spawn(|_| Err("no good".into()));
        assert_eq!(drain(&job).1, Err("no good".to_string()));
    }

    #[test]
    fn a_panicking_job_becomes_an_error_instead_of_hanging_the_screen() {
        let job: Job<u32> = Job::spawn(|_| panic!("boom"));
        assert!(drain(&job).1.unwrap_err().contains("crashed"));
    }

    #[test]
    fn a_panicking_job_keeps_the_panic_message() {
        let job: Job<u32> = Job::spawn(|_| panic!("disk {} exploded", 7));
        let e = drain(&job).1.unwrap_err();
        assert!(e.contains("crashed") && e.contains("disk 7 exploded"), "{}", e);
        let job: Job<u32> = Job::spawn(|_| panic!("static text"));
        assert!(drain(&job).1.unwrap_err().contains("static text"));
    }

    #[test]
    fn a_job_runs_on_a_thread_named_tui_job() {
        let job: Job<String> = Job::spawn(|_| Ok(std::thread::current().name().map(String::from).unwrap_or_default()));
        assert_eq!(drain_any(&job), Ok("tui-job".to_string()));
    }

    fn drain_any<T>(job: &Job<T>) -> Result<T, String> {
        for _ in 0..500 {
            while let Some(msg) = job.try_next() {
                if let Msg::Done(r) = msg {
                    return r;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the job never finished");
    }

    #[test]
    fn try_next_does_not_block_when_nothing_has_arrived() {
        let (_tx, rx) = std::sync::mpsc::channel::<Msg<u32>>();
        let job = Job::from_receiver(rx);
        assert!(job.try_next().is_none());
    }
}
