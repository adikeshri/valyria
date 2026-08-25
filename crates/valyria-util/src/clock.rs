//! `Clock` (determinism convention, §3): injected everywhere a timestamp is
//! read, so agent-loop and journal-replay tests can run against a
//! deterministic clock instead of real wall time.

use parking_lot::Mutex;
use std::sync::Arc;

use valyria_types::Timestamp;

pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock that only advances when told to. Every call to `now()` returns
/// the same value until [`FixedClock::advance`] is called — this is what
/// makes journal-replay tests byte-for-byte reproducible.
#[derive(Clone)]
pub struct FixedClock(Arc<Mutex<u128>>);

impl FixedClock {
    pub fn at_millis(millis: u128) -> Self {
        Self(Arc::new(Mutex::new(millis)))
    }

    pub fn advance(&self, millis: u128) {
        *self.0.lock() += millis;
    }

    pub fn set_millis(&self, millis: u128) {
        *self.0.lock() = millis;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(*self.0.lock())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_does_not_advance_on_its_own() {
        let clock = FixedClock::at_millis(1_000);
        assert_eq!(clock.now(), Timestamp::from_millis(1_000));
        assert_eq!(clock.now(), Timestamp::from_millis(1_000));
    }

    #[test]
    fn fixed_clock_advances_explicitly() {
        let clock = FixedClock::at_millis(1_000);
        clock.advance(500);
        assert_eq!(clock.now(), Timestamp::from_millis(1_500));
    }

    #[test]
    fn cloned_fixed_clocks_share_state() {
        let a = FixedClock::at_millis(0);
        let b = a.clone();
        b.advance(42);
        assert_eq!(a.now(), Timestamp::from_millis(42));
    }

    #[test]
    fn system_clock_moves_forward() {
        let clock = SystemClock;
        let t1 = clock.now();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t2 = clock.now();
        assert!(t2 >= t1);
    }
}
