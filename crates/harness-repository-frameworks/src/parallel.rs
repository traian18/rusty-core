//! Runs a function over many files on all available cores and returns the results in input
//! order, so the output does not depend on scheduling.
use std::thread;

pub fn map<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(items.len().max(1));
    if threads <= 1 || items.len() < 32 {
        return items.iter().map(&work).collect();
    }
    let chunk = items.len().div_ceil(threads);
    thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|part| scope.spawn(|| part.iter().map(&work).collect::<Vec<R>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a framework worker panicked"))
            .collect()
    })
}
