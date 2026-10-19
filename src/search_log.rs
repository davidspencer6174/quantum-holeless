use std::cell::{Cell, RefCell};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) fn warn_output(path: &Path, error: &io::Error) {
    static WARNED: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    if WARNED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_owned())
    {
        write(format_args!("[output:warning] Cannot refresh {}: {error}. Search continues; this readable export may be stale.\n", path.display()));
    }
}

#[derive(Clone)]
pub(crate) struct SearchLog(Arc<Mutex<File>>);

impl SearchLog {
    pub(crate) fn create(path: &Path) -> io::Result<Self> {
        Ok(Self(Arc::new(Mutex::new(
            OpenOptions::new().write(true).create_new(true).open(path)?,
        ))))
    }

    pub(crate) fn line(&self, message: &str) -> io::Result<()> {
        let mut file = self.0.lock().unwrap_or_else(|e| e.into_inner());
        writeln!(file, "{message}")?;
        file.flush()
    }
}

thread_local! {
    static WORKER: Cell<usize> = const { Cell::new(0) };
    static OUTPUT: RefCell<Option<SearchLog>> = const { RefCell::new(None) };
}

pub(crate) fn worker_id() -> usize {
    WORKER.with(Cell::get)
}

pub(crate) struct LogScope {
    previous_worker: usize,
    previous_output: Option<SearchLog>,
    thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

pub(crate) fn enter(worker: usize, output: SearchLog) -> LogScope {
    LogScope {
        previous_worker: WORKER.with(|id| id.replace(worker)),
        previous_output: OUTPUT.with(|sink| sink.replace(Some(output))),
        thread_bound: std::marker::PhantomData,
    }
}

impl Drop for LogScope {
    fn drop(&mut self) {
        WORKER.with(|id| id.set(self.previous_worker));
        OUTPUT.with(|sink| sink.replace(self.previous_output.take()));
    }
}

pub(crate) fn write(args: fmt::Arguments<'_>) {
    OUTPUT.with(|output| {
        if let Some(sink) = output.borrow().as_ref() {
            sink.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .write_fmt(args)
                .expect("failed to write search log");
        } else {
            std::print!("{args}");
        }
    });
}

macro_rules! search_print {
    ($($arg:tt)*) => { $crate::search_log::write(format_args!($($arg)*)) };
}
macro_rules! search_println {
    () => { $crate::search_log::write(format_args!("\n")) };
    ($($arg:tt)*) => { $crate::search_log::write(format_args!("{}\n", format_args!($($arg)*))) };
}
pub(crate) use search_print as print;
pub(crate) use search_println as println;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solver_output_stays_with_its_worker_and_scope_restores_routing() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scratch/tests-worker-logs")
            .join(format!("{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::thread::scope(|scope| {
            for id in 1..=2 {
                let path = directory.join(format!("worker-{id}.log"));
                scope.spawn(move || {
                    let sink = SearchLog::create(&path).unwrap();
                    {
                        let _routing = enter(id, sink);
                        assert_eq!(worker_id(), id);
                        crate::search_log::println!("owned by worker {id}");
                        let parsed = crate::io::parse_input("exit 3,1\nsteps 2\n4200\n").unwrap();
                        crate::bfs::forward_bfs(
                            &parsed.board,
                            parsed.exit,
                            2,
                            &[],
                            &[],
                            false,
                            1,
                            None,
                        );
                    }
                    assert_eq!(worker_id(), 0);
                });
            }
        });
        for id in 1..=2 {
            let text = std::fs::read_to_string(directory.join(format!("worker-{id}.log"))).unwrap();
            assert!(text.contains(&format!("owned by worker {id}")));
            assert!(!text.contains(&format!("owned by worker {}", 3 - id)));
            assert!(text.contains("Starting forward BFS"));
            assert!(text.contains("Initial Board"));
        }
    }
}
