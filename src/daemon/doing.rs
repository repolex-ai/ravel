//! What raveld is doing right now, kept so the memory watch can say it.
//!
//! On 2026-11-21 raveld's memory rose by gigabytes twice (09:02, 09:12) with
//! no log line to say why: a sync pass, a memory pass and a request each log
//! only when they finish, and only some of them. Each now holds a [`Doing`]
//! while it runs, and the watch in `http` writes the list when memory is high.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

fn board() -> &'static Mutex<HashMap<u64, (Instant, String)>> {
    static B: OnceLock<Mutex<HashMap<u64, (Instant, String)>>> = OnceLock::new();
    B.get_or_init(Default::default)
}

/// One thing under way; gone from the list when dropped.
pub struct Doing(u64);

/// Put `what` on the list until the returned value is dropped.
pub fn now(what: impl Into<String>) -> Doing {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut b) = board().lock() {
        b.insert(id, (Instant::now(), what.into()));
    }
    Doing(id)
}

impl Drop for Doing {
    fn drop(&mut self) {
        if let Ok(mut b) = board().lock() {
            b.remove(&self.0);
        }
    }
}

/// Everything under way, longest-running first: `what (for 12s)`.
pub fn list() -> Vec<String> {
    let Ok(b) = board().lock() else {
        return Vec::new();
    };
    let mut all: Vec<_> = b.values().cloned().collect();
    all.sort_by_key(|(t, _)| *t);
    all.into_iter()
        .map(|(t, what)| format!("{what} (for {:.0}s)", t.elapsed().as_secs_f64()))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_thing_is_listed_while_it_runs_and_not_after() {
        let d = super::now("counting sheep 7f3a");
        assert!(super::list().iter().any(|l| l.starts_with("counting sheep 7f3a (for ")));
        drop(d);
        assert!(!super::list().iter().any(|l| l.contains("counting sheep 7f3a")));
    }
}
