use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) fn thread_count(max_threads: u8) -> usize {
    let available = if HAS_THREADS {
        std::thread::available_parallelism().map_or(1, NonZeroUsize::get)
    } else {
        1
    };
    match max_threads {
        0 => available,
        max_threads => usize::from(max_threads).min(available),
    }
}

pub(crate) const HAS_THREADS: bool = !cfg!(target_family = "wasm");

/// Results in index order.
pub(crate) fn par_map<T: Send, F: Fn(usize) -> T + Sync>(
    threads: usize,
    count: usize,
    job: F,
) -> Vec<T> {
    if threads <= 1 || count <= 1 || !HAS_THREADS {
        return (0..count).map(job).collect();
    }
    let next = AtomicUsize::new(0);
    let done: Vec<Vec<(usize, T)>> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.min(count))
            .map(|_| {
                scope.spawn(|| {
                    let mut results = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= count {
                            break results;
                        }
                        results.push((index, job(index)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    let mut out: Vec<Option<T>> = Vec::from_fn(count, |_| None);
    for (index, value) in done.into_iter().flatten() {
        if let Some(slot) = out.get_mut(index) {
            *slot = Some(value);
        }
    }
    out.into_iter()
        .map(|value| value.expect("every index is filled"))
        .collect()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    #[test]
    fn thread_count_is_capped_at_the_cores() {
        let cores = super::thread_count(0);
        assert!(cores >= 1);
        assert_eq!(super::thread_count(1), 1);
        assert_eq!(super::thread_count(u8::MAX), cores.min(255));
    }

    #[test]
    fn par_map_order() {
        let doubled = super::par_map(3, 100, |index| index * 2);
        assert_eq!(doubled, (0..100).map(|index| index * 2).collect::<Vec<_>>());
    }
}
