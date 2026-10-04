//! The encoders' frame-level threading: whole frames are independent once
//! the frame number is fixed, so a batch of them is coded on scoped
//! threads and handed back in order. The bytes are the same whatever the
//! thread count.

use std::sync::atomic::{AtomicUsize, Ordering};

/// The thread count `0` ("automatic") stands for: the machine's.
pub(crate) fn auto_threads() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// `f(0) … f(count - 1)` in order, on up to `threads` threads (the calling
/// thread among them). Items are handed out one at a time, so uneven ones
/// balance.
pub(crate) fn map<T: Send>(count: usize, threads: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let threads = threads.min(count);
    if threads <= 1 {
        return (0..count).map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let work = || {
        let mut done = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= count {
                return done;
            }
            done.push((i, f(i)));
        }
    };
    let mut all: Vec<(usize, T)> = std::thread::scope(|s| {
        let helpers: Vec<_> = (1..threads).map(|_| s.spawn(work)).collect();
        let mut all = work();
        for h in helpers {
            all.extend(h.join().expect("an encoder thread panicked"));
        }
        all
    });
    all.sort_unstable_by_key(|(i, _)| *i);
    all.into_iter().map(|(_, v)| v).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn results_come_back_in_order() {
        for threads in [0, 1, 2, 7, 64] {
            let got = super::map(50, threads, |i| i * i);
            assert_eq!(
                got,
                (0..50).map(|i| i * i).collect::<Vec<_>>(),
                "{threads} threads"
            );
        }
        assert!(super::map(0, 4, |i| i).is_empty());
    }
}
