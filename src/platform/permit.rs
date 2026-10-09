//! Bounded slots: a shared counter capped at a maximum, released on drop.
use std::sync::atomic::{AtomicUsize, Ordering};

/// One taken slot of `counter`; dropping it gives the slot back.
#[must_use = "the slot is released when the permit is dropped"]
pub struct Permit(&'static AtomicUsize);

impl Permit {
    /// Takes a slot while fewer than `max` are taken, else `None`.
    pub fn try_acquire(counter: &'static AtomicUsize, max: usize) -> Option<Self> {
        counter
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()
            .map(|_| Self(counter))
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_are_capped_and_released_on_drop() {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let permits: Vec<_> = std::iter::from_fn(|| Permit::try_acquire(&COUNTER, 3))
            .take(4)
            .collect();
        assert_eq!(permits.len(), 3);
        assert_eq!(COUNTER.load(Ordering::SeqCst), 3);
        assert!(Permit::try_acquire(&COUNTER, 3).is_none());
        drop(permits);
        assert_eq!(COUNTER.load(Ordering::SeqCst), 0);
        assert!(Permit::try_acquire(&COUNTER, 3).is_some());
        assert_eq!(COUNTER.load(Ordering::SeqCst), 0);
    }
}
