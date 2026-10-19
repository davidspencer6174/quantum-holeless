use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

struct QueueState<T> {
    jobs: VecDeque<T>,
    closed: bool,
}

pub(crate) struct WorkQueue<T> {
    state: Mutex<QueueState<T>>,
    ready: Condvar,
    space: Condvar,
    capacity: usize,
    cancelled: AtomicBool,
}

impl<T> WorkQueue<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            state: Mutex::new(QueueState {
                jobs: VecDeque::new(),
                closed: false,
            }),
            ready: Condvar::new(),
            space: Condvar::new(),
            capacity,
            cancelled: AtomicBool::new(false),
        }
    }

    pub(crate) fn push(&self, job: T) -> bool {
        let mut state = self.state.lock().unwrap();
        while state.jobs.len() == self.capacity && !state.closed {
            state = self.space.wait(state).unwrap();
        }
        if state.closed {
            return false;
        }
        state.jobs.push_back(job);
        self.ready.notify_one();
        true
    }

    pub(crate) fn pop(&self) -> Option<T> {
        let mut state = self.state.lock().unwrap();
        while state.jobs.is_empty() && !state.closed {
            state = self.ready.wait(state).unwrap();
        }
        if self.is_cancelled() {
            return None;
        }
        let job = state.jobs.pop_front();
        self.space.notify_one();
        job
    }

    pub(crate) fn finish(&self) {
        self.state.lock().unwrap().closed = true;
        self.ready.notify_all();
        self.space.notify_all();
    }

    pub(crate) fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        self.cancelled.store(true, Ordering::Release);
        state.jobs.clear();
        state.closed = true;
        self.ready.notify_all();
        self.space.notify_all();
    }

    pub(crate) fn discard_pending(&self) {
        let mut state = self.state.lock().unwrap();
        state.jobs.clear();
        state.closed = true;
        self.ready.notify_all();
        self.space.notify_all();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_drains_in_fifo_order() {
        let queue = WorkQueue::new(3);
        for job in 0..3 {
            assert!(queue.push(job));
        }
        queue.finish();
        assert!(!queue.push(4));
        assert_eq!(
            (0..3).map(|_| queue.pop().unwrap()).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn cancellation_releases_a_blocked_producer() {
        let queue = WorkQueue::new(1);
        queue.push(0);
        std::thread::scope(|scope| {
            let producer = scope.spawn(|| queue.push(1));
            queue.cancel();
            assert!(!producer.join().unwrap());
        });
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn workers_take_each_job_once_and_in_increasing_order() {
        let queue = WorkQueue::new(2);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        let mut jobs = Vec::new();
                        while let Some(job) = queue.pop() {
                            jobs.push(job);
                        }
                        assert!(jobs.windows(2).all(|w| w[0] < w[1]));
                        jobs
                    })
                })
                .collect();
            for job in 0..100 {
                assert!(queue.push(job));
            }
            queue.finish();
            let mut jobs: Vec<_> = workers
                .into_iter()
                .flat_map(|w| w.join().unwrap())
                .collect();
            jobs.sort_unstable();
            assert_eq!(jobs, (0..100).collect::<Vec<_>>());
        });
    }
}
