//! Bounded scoped-thread fan-out whose results never depend on scheduling.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Applies `operation` to every item with at most `workers` threads and returns results in item
/// order, so callers observe the same sequence for every worker count.
pub fn map_ordered<T, R, F>(items: &[T], workers: usize, operation: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let workers = workers.clamp(1, items.len().max(1));
    if workers == 1 {
        return items.iter().map(operation).collect();
    }
    let next = AtomicUsize::new(0);
    let mut indexed = std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut produced = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            break;
                        };
                        produced.push((index, operation(item)));
                    }
                    produced
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| match handle.join() {
                Ok(produced) => produced,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect::<Vec<_>>()
    });
    indexed.sort_unstable_by_key(|(index, _)| *index);
    indexed.into_iter().map(|(_, result)| result).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_ordered_should_return_item_order_for_every_worker_count() {
        let items = (0..257_u64).collect::<Vec<_>>();
        let expected = items.iter().map(|value| value * 3).collect::<Vec<_>>();

        for workers in [1, 2, 7, 64, 1_000] {
            assert_eq!(map_ordered(&items, workers, |value| value * 3), expected);
        }
    }

    #[test]
    fn map_ordered_should_accept_empty_input() {
        let items: [u8; 0] = [];

        assert_eq!(map_ordered(&items, 8, |value| *value), Vec::<u8>::new());
    }
}
