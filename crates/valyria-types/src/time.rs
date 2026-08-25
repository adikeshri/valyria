//! A minimal timestamp type so domain types don't need a chrono/time
//! dependency. Represents milliseconds since the Unix epoch, UTC.
//!
//! Deliberately does not read the system clock itself outside of
//! [`Timestamp::now`] — everywhere that determinism matters (journal
//! entries, tests), callers should go through the `Clock` trait in
//! `valyria-util`, which this type is designed to be produced by.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub u128);

impl Timestamp {
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before the Unix epoch")
            .as_millis();
        Self(millis)
    }

    pub fn from_millis(millis: u128) -> Self {
        Self(millis)
    }

    pub fn as_millis(&self) -> u128 {
        self.0
    }

    pub fn saturating_duration_since(&self, earlier: Timestamp) -> u128 {
        self.0.saturating_sub(earlier.0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t+{}ms", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_after_epoch() {
        assert!(Timestamp::now().as_millis() > 0);
    }

    #[test]
    fn ordering_matches_millis() {
        let a = Timestamp::from_millis(100);
        let b = Timestamp::from_millis(200);
        assert!(a < b);
        assert_eq!(b.saturating_duration_since(a), 100);
    }

    #[test]
    fn saturating_duration_never_underflows() {
        let a = Timestamp::from_millis(100);
        let b = Timestamp::from_millis(200);
        assert_eq!(a.saturating_duration_since(b), 0);
    }
}
