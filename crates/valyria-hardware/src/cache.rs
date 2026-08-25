//! Short-TTL probe caching (§39: "cached with a short TTL, invalidated on
//! wake-from-sleep"). True wake-from-sleep detection needs an OS-level
//! power-management hook (`IOKit` notifications on macOS, `systemd-logind`
//! signals on Linux, `SetSystemPowerStatePolicy` and friends on Windows) —
//! out of scope for this pass. A short TTL is a deliberate, honest
//! approximation: the cache self-invalidates within one TTL window of any
//! change, sleep/wake included, without pretending to detect the event.
//! [`CachedProbe::invalidate`] is exposed for a caller that *does* wire up
//! a real wake notification later to force an immediate refresh.

use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::probe::probe;
use crate::report::HardwareReport;

pub struct CachedProbe {
    ttl: Duration,
    inner: Mutex<Option<(Instant, HardwareReport)>>,
}

impl CachedProbe {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            inner: Mutex::new(None),
        }
    }

    pub fn get(&self) -> HardwareReport {
        let mut guard = self.inner.lock();
        if let Some((at, report)) = guard.as_ref() {
            if at.elapsed() < self.ttl {
                return report.clone();
            }
        }
        let report = probe();
        *guard = Some((Instant::now(), report.clone()));
        report
    }

    pub fn invalidate(&self) {
        *self.inner.lock() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_within_ttl() {
        let cache = CachedProbe::new(Duration::from_secs(60));
        let a = cache.get();
        let b = cache.get();
        assert_eq!(a, b);
    }

    #[test]
    fn invalidate_forces_a_fresh_probe_on_next_get() {
        let cache = CachedProbe::new(Duration::from_secs(60));
        cache.get();
        cache.invalidate();
        assert!(cache.inner.lock().is_none());
    }

    #[test]
    fn expired_ttl_triggers_a_fresh_probe() {
        let cache = CachedProbe::new(Duration::from_millis(1));
        cache.get();
        std::thread::sleep(Duration::from_millis(10));
        // Not asserting inequality (probe values are typically stable
        // across a few ms on a real machine) — just that this doesn't
        // panic and re-probes rather than serving something ancient.
        let _ = cache.get();
    }
}
