//! Content hashing (D6, D7): `blake3` everywhere a content hash is needed —
//! optimistic-concurrency preconditions on writes, the content-addressed
//! blob store, and index cache keys.

use std::fmt;
use std::io::Read;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(#[serde(with = "hex_serde")] [u8; 32]);

impl ContentHash {
    pub fn of_bytes(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }

    /// Hash a reader's contents without buffering the whole thing in memory
    /// at once — used for large tool output and downloaded model files.
    pub fn of_reader<R: Read>(mut reader: R) -> std::io::Result<Self> {
        let mut hasher = blake3::Hasher::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(Self(*hasher.finalize().as_bytes()))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid content hash: {0}")]
pub struct HashParseError(String);

impl FromStr for ContentHash {
    type Err = HashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex_decode(s).ok_or_else(|| HashParseError(s.to_string()))?;
        if bytes.len() != 32 {
            return Err(HashParseError(s.to_string()));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

mod hex_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::hex_encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let bytes = super::hex_decode(&s).ok_or_else(|| serde::de::Error::custom("invalid hex"))?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("expected 32 bytes"));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_bytes_hash_identically() {
        assert_eq!(
            ContentHash::of_bytes(b"hello"),
            ContentHash::of_bytes(b"hello")
        );
    }

    #[test]
    fn different_bytes_hash_differently() {
        assert_ne!(
            ContentHash::of_bytes(b"hello"),
            ContentHash::of_bytes(b"world")
        );
    }

    #[test]
    fn hex_round_trips() {
        let h = ContentHash::of_bytes(b"round trip me");
        let hex = h.to_hex();
        let parsed: ContentHash = hex.parse().unwrap();
        assert_eq!(h, parsed);
    }

    #[test]
    fn reader_hash_matches_bytes_hash() {
        let data = b"the quick brown fox jumps over the lazy dog".repeat(100);
        let via_bytes = ContentHash::of_bytes(&data);
        let via_reader = ContentHash::of_reader(std::io::Cursor::new(&data)).unwrap();
        assert_eq!(via_bytes, via_reader);
    }

    #[test]
    fn rejects_bad_hex() {
        assert!("not-hex".parse::<ContentHash>().is_err());
        assert!("aa".parse::<ContentHash>().is_err()); // too short
    }

    #[test]
    fn serde_round_trip() {
        let h = ContentHash::of_bytes(b"serde me");
        let json = serde_json::to_string(&h).unwrap();
        let back: ContentHash = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
    }
}
