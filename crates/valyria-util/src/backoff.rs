//! Exponential backoff with jitter, used by anything that retries: model
//! calls, tool execution, download resumption. A plain iterator so callers
//! can `for delay in Backoff::new(..)` without pulling in a specific retry
//! framework.

use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    attempt: u32,
    base: Duration,
    max: Duration,
    max_attempts: Option<u32>,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            attempt: 0,
            base,
            max,
            max_attempts: None,
        }
    }

    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = Some(max_attempts);
        self
    }

    /// The delay for the *next* attempt without consuming it — useful for
    /// logging "retrying in Xms" before actually sleeping.
    pub fn peek(&self) -> Duration {
        let factor = 2u32.saturating_pow(self.attempt.min(20));
        self.base.saturating_mul(factor).min(self.max)
    }

    pub fn attempt_number(&self) -> u32 {
        self.attempt
    }

    /// Deterministic jitter derived from the attempt number, so tests don't
    /// need to inject an RNG to get reproducible output while still
    /// avoiding a thundering herd across many concurrent callers with the
    /// same base/max (each caller's attempt sequence is the same, but real
    /// callers are staggered in wall-clock start time already).
    fn jitter(&self, delay: Duration) -> Duration {
        if delay.is_zero() {
            return delay;
        }
        // Full jitter: uniform in [0, delay], seeded from the attempt index
        // via a cheap integer hash rather than pulling in `rand` here.
        let seed = (self.attempt as u64).wrapping_mul(0x9E3779B97F4A7C15);
        let frac = (seed >> 40) as f64 / (1u64 << 24) as f64; // in [0, 1)
        delay.mul_f64(frac.clamp(0.0, 1.0))
    }
}

impl Iterator for Backoff {
    type Item = Duration;

    fn next(&mut self) -> Option<Duration> {
        if let Some(max) = self.max_attempts {
            if self.attempt >= max {
                return None;
            }
        }
        let delay = self.jitter(self.peek());
        self.attempt += 1;
        Some(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_and_cap_at_max() {
        let backoff = Backoff::new(Duration::from_millis(10), Duration::from_millis(200));
        let delays: Vec<Duration> = backoff.take(10).collect();
        for d in &delays {
            assert!(*d <= Duration::from_millis(200));
        }
        // later peeks (pre-jitter) should reach the cap
        let mut probe = Backoff::new(Duration::from_millis(10), Duration::from_millis(200));
        for _ in 0..10 {
            probe.next();
        }
        assert_eq!(probe.peek(), Duration::from_millis(200));
    }

    #[test]
    fn respects_max_attempts() {
        let backoff =
            Backoff::new(Duration::from_millis(1), Duration::from_millis(100)).with_max_attempts(3);
        assert_eq!(backoff.count(), 3);
    }

    #[test]
    fn is_deterministic() {
        let a: Vec<_> = Backoff::new(Duration::from_millis(5), Duration::from_secs(1))
            .take(5)
            .collect();
        let b: Vec<_> = Backoff::new(Duration::from_millis(5), Duration::from_secs(1))
            .take(5)
            .collect();
        assert_eq!(a, b);
    }

    #[test]
    fn unbounded_by_default() {
        let backoff = Backoff::new(Duration::from_millis(1), Duration::from_millis(10));
        assert_eq!(backoff.take(1000).count(), 1000);
    }
}
