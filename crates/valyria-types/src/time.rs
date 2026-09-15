//! A minimal timestamp type so domain types don't need a chrono/time
//! dependency. Represents milliseconds since the Unix epoch, UTC.
//!
//! Deliberately does not read the system clock itself outside of
//! [`Timestamp::now`] — everywhere that determinism matters (journal
//! entries, tests), callers should go through the `Clock` trait in
//! `valyria-util`, which this type is designed to be produced by.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub u128);

/// Serialized as a decimal string, not a JSON number: `serde_json` has no
/// native u128 support (`serialize_u128`/`deserialize_u128` both error
/// with "u128 is not supported" unless the `arbitrary_precision` feature
/// is enabled, which this workspace doesn't do). Discovered when M5's
/// role pipeline became the first code to ever call `serde_json::
/// to_string` on a value containing a `Timestamp` (`PlanRevision`, inside
/// `Artifact::Plan`) — every earlier caller happened to persist a
/// `Timestamp` through a raw SQL column (`as_millis() as i64`) rather than
/// through `serde`, so this was latent, not exercised. A string loses
/// nothing (full `u128` range, no precision limit the way a JSON number
/// backed by `f64` would) and costs nothing callers here care about.
impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse::<u128>()
            .map(Timestamp)
            .map_err(|e| serde::de::Error::custom(format!("invalid Timestamp {s:?}: {e}")))
    }
}

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

    /// Regression test (M5): a bare `#[serde(transparent)]` u128 newtype
    /// serializes fine through most `serde` backends but always errors
    /// through `serde_json` specifically ("u128 is not supported") unless
    /// `arbitrary_precision` is enabled. Every direct `serde_json` round
    /// trip of a `Timestamp` must work regardless of which crate embeds it.
    #[test]
    fn round_trips_through_serde_json() {
        let t = Timestamp::from_millis(1_732_000_000_123);
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"1732000000123\"");
        let back: Timestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn rejects_a_non_numeric_string() {
        let err = serde_json::from_str::<Timestamp>("\"not a number\"").unwrap_err();
        assert!(err.to_string().contains("invalid Timestamp"));
    }
}
