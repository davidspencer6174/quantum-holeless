use std::io::{self, Write};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::types::Dir;

pub(crate) const REPORT_INTERVAL: Duration = Duration::from_secs(10 * 60);

pub(crate) struct SearchProgress {
    started: Instant,
    phase_started: Instant,
    dimensions: (u16, u16),
    exit: (u16, u16),
    path: Vec<Dir>,
    phase: &'static str,
}

impl SearchProgress {
    pub(crate) fn new(width: u16, height: u16, exit: (u16, u16)) -> Self {
        let now = Instant::now();
        Self {
            started: now,
            phase_started: now,
            dimensions: (width, height),
            exit,
            path: Vec::new(),
            phase: "backward",
        }
    }

    pub(crate) fn update(&mut self, path: &[Dir], phase: &'static str) {
        self.path.clear();
        self.path.extend_from_slice(path);
        self.set_phase(phase);
    }

    pub(crate) fn set_phase(&mut self, phase: &'static str) {
        self.phase = phase;
        self.phase_started = Instant::now();
    }

    pub(crate) fn message(
        &self,
        reason: &str,
        boards: usize,
        best: usize,
        slowest_ms: u64,
    ) -> String {
        let elapsed = self.started.elapsed().as_secs();
        let moves: String = self
            .path
            .iter()
            .rev()
            .map(|d| direction_char(d.opposite()))
            .collect();
        let solution: String = self.path.iter().copied().map(direction_char).collect();
        format!(
            "[progress:{reason}] board={}x{} exit={},{} elapsed={:02}:{:02}:{:02} \
             depth={} boards={boards} best={best} slowest_bfs_ms={slowest_ms} \
             phase={} phase_elapsed_s={} moves={} solution={}",
            self.dimensions.0,
            self.dimensions.1,
            self.exit.0,
            self.exit.1,
            elapsed / 3600,
            elapsed / 60 % 60,
            elapsed % 60,
            self.path.len(),
            self.phase,
            self.phase_started.elapsed().as_secs(),
            if moves.is_empty() { "-" } else { &moves },
            if solution.is_empty() { "-" } else { &solution },
        )
    }
}

fn direction_char(dir: Dir) -> char {
    match dir {
        Dir::Up => 'U',
        Dir::Down => 'D',
        Dir::Left => 'L',
        Dir::Right => 'R',
    }
}

pub(crate) fn print_progress(message: String) {
    crate::search_log::println!("{message}");
    let _ = io::stdout().flush();
}

pub(crate) struct ProgressReporter {
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl ProgressReporter {
    pub(crate) fn start(
        interval: Duration,
        snapshot: impl Fn() -> String + Send + 'static,
        mut emit: impl FnMut(String) + Send + 'static,
    ) -> Self {
        let (stop, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = receiver.recv_timeout(interval) {
                emit(snapshot());
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for ProgressReporter {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_string_reverses_and_inverts_the_solution() {
        let mut progress = SearchProgress::new(5, 7, (2, 3));
        assert!(progress
            .message("shallow", 1, 0, 0)
            .contains("moves=- solution=-"));
        progress.update(&[Dir::Right, Dir::Down, Dir::Left], "forward-solve");
        let message = progress.message("periodic", 1234, 69, 250);
        assert!(message.contains("board=5x7 exit=2,3"));
        assert!(message.contains("depth=3 boards=1234 best=69"));
        assert!(message.contains("phase=forward-solve"));
        assert!(message.ends_with("moves=RUL solution=RDL"));
    }

    #[test]
    fn periodic_report_does_not_require_search_activity() {
        let (sent, received) = mpsc::channel();
        let reporter = ProgressReporter::start(
            Duration::from_millis(10),
            || "search is still solving this board".to_string(),
            move |message| {
                let _ = sent.send(message);
            },
        );
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            "search is still solving this board"
        );
        drop(reporter);
        while received.try_recv().is_ok() {}
        assert_eq!(
            received.recv_timeout(Duration::from_millis(50)),
            Err(RecvTimeoutError::Disconnected)
        );
    }
}
