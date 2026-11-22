use std::num::NonZeroUsize;
use std::sync::{Mutex, PoisonError};

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

/// Results in item order.
pub(crate) fn par_map<I, T, F>(threads: usize, items: I, job: F) -> Vec<T>
where
    I: IntoIterator,
    I::IntoIter: ExactSizeIterator + Send,
    I::Item: Send,
    T: Send,
    F: Fn(I::Item) -> T + Sync,
{
    let items = items.into_iter();
    let count = items.len();
    if threads <= 1 || count <= 1 || !HAS_THREADS {
        return items.map(job).collect();
    }
    let next = Mutex::new(items.enumerate());
    let mut done: Vec<(usize, T)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.min(count))
            .map(|_| {
                scope.spawn(|| {
                    let mut results = Vec::new();
                    loop {
                        let Some((index, item)) =
                            next.lock().unwrap_or_else(PoisonError::into_inner).next()
                        else {
                            break results;
                        };
                        results.push((index, job(item)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    done.sort_unstable_by_key(|&(index, _)| index);
    done.into_iter().map(|(_, value)| value).collect()
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
        let doubled = super::par_map(3, 0..100, |index| index * 2);
        assert_eq!(doubled, (0..100).map(|index| index * 2).collect::<Vec<_>>());
    }
}
