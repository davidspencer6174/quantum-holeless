//! Opt-in inclusive timings; normal builds contain no profiling calls.
use std::{cell::RefCell, time::Instant};
use crate::search_log::println;

const NAMES: [&str; 5] = [
    "imputation",
    "witness refresh",
    "assignment search",
    "relaxed solve",
    "expansion",
];
thread_local! {
    static COUNTERS: RefCell<[(u64, u128); 5]> = const { RefCell::new([(0, 0); 5]) };
}

pub struct Timer(usize, Instant);
impl Timer {
    pub fn start(phase: usize) -> Self {
        Self(phase, Instant::now())
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        let elapsed = self.1.elapsed().as_nanos();
        COUNTERS.with(|c| {
            let mut c = c.borrow_mut();
            c[self.0].0 += 1;
            c[self.0].1 += elapsed;
        });
    }
}

pub fn reset() {
    COUNTERS.with(|c| *c.borrow_mut() = [(0, 0); 5]);
}
pub fn report() {
    COUNTERS.with(|c| {
        for (name, &(calls, ns)) in NAMES.iter().zip(c.borrow().iter()) {
            println!(
                "profile: {name}: {calls} calls, {:.3} ms inclusive",
                ns as f64 / 1e6
            );
        }
    });
}
