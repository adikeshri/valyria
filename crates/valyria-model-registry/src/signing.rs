//! Detached ed25519 signatures over the catalog's raw JSON bytes (§ M6
//! "signed catalog refresh"). The mechanism a refreshed catalog must pass
//! before [`crate::Catalog`] will touch it: verify the signature against
//! a trusted public key, then refuse anything whose declared `version`
//! isn't strictly newer than what's already cached — a validly-signed
//! but *stale* catalog is a replay/rollback attack, not a legitimate
//! refresh, and the version check is what catches it (an attacker who
//! can't forge a signature can still replay an old, once-legitimately-
//! signed one otherwise).
//!
//! Deliberately signs the exact bytes on the wire, not a re-serialized
//! form of the parsed structure — JSON has no single canonical byte
//! representation, so verifying anything other than what was actually
//! transmitted would open a (probably harmless, but needless)
//! malleability gap between "what was signed" and "what gets parsed".

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::error::{RegistryError, Result};

/// The public key this build trusts for a `catalog_refresh` (hex-encoded,
/// 32 raw bytes). **Placeholder**: generated for this mechanism to exist
/// and be testable end-to-end; no real catalog-hosting/signing pipeline
/// exists yet to actually publish anything under it (see `docs/
/// COMPLETION-PLAN.md`'s M6 section). Whoever stands that pipeline up
/// generates a fresh keypair with [`generate_keypair`], keeps the private
/// key *out* of this repository entirely, and replaces this constant
/// with the new public half.
pub const CATALOG_PUBLIC_KEY_HEX: &str =
    "da63f39d753c274669b0358047f78cdf5aa1d5befd2a5f1743b912b418285d96";

/// Generate a fresh signing keypair. Used by catalog-signing tooling and
/// by tests that need to produce a validly-signed fixture without
/// depending on (or risking exposure of) any real key material.
pub fn generate_keypair() -> SigningKey {
    SigningKey::generate(&mut rand::rngs::OsRng)
}

/// Sign `bytes` (a catalog's raw JSON) with `key`, returning a
/// hex-encoded detached signature suitable for a `.sig` sidecar file.
pub fn sign(key: &SigningKey, bytes: &[u8]) -> String {
    let sig: Signature = key.sign(bytes);
    hex::encode(sig.to_bytes())
}

/// Parse a hex-encoded 32-byte ed25519 public key. `Err` for anything
/// that isn't exactly that shape — a malformed trusted key is a
/// programmer error (a bad constant, a bad `--pubkey` flag), never a
/// legitimate "no key configured" state.
pub fn parse_public_key_hex(hex_str: &str) -> Result<VerifyingKey> {
    let bytes = hex::decode(hex_str).map_err(|e| RegistryError::MalformedCatalog {
        detail: format!("public key is not valid hex: {e}"),
    })?;
    let bytes: [u8; 32] =
        bytes
            .try_into()
            .map_err(|v: Vec<u8>| RegistryError::MalformedCatalog {
                detail: format!("public key must be 32 bytes, got {}", v.len()),
            })?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| RegistryError::MalformedCatalog {
        detail: format!("not a valid ed25519 public key: {e}"),
    })
}

/// Verify `signature_hex` over `bytes` against `public_key`. `Err(
/// BadSignature)` on any failure to decode or verify — never partial
/// credit, never a "probably fine" path.
pub fn verify(public_key: &VerifyingKey, bytes: &[u8], signature_hex: &str) -> Result<()> {
    let sig_bytes = hex::decode(signature_hex.trim()).map_err(|_| RegistryError::BadSignature)?;
    let sig_bytes: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| RegistryError::BadSignature)?;
    let signature = Signature::from_bytes(&sig_bytes);
    public_key
        .verify(bytes, &signature)
        .map_err(|_| RegistryError::BadSignature)
}

/// Minimal hex codec — pulling in a whole crate for `encode`/`decode`
/// would be disproportionate to the ~15 lines it takes here, and this
/// module is the only place in the crate that needs it.
mod hex {
    pub fn encode(bytes: [u8; 64]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn decode(s: &str) -> std::result::Result<Vec<u8>, String> {
        if !s.len().is_multiple_of(2) {
            return Err("odd-length hex string".into());
        }
        (0..s.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("invalid hex byte: {e}"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_in_public_key_parses() {
        parse_public_key_hex(CATALOG_PUBLIC_KEY_HEX).expect("constant must be a valid pubkey");
    }

    #[test]
    fn sign_then_verify_round_trips() {
        let key = generate_keypair();
        let bytes = b"a catalog's worth of json bytes";
        let sig = sign(&key, bytes);
        verify(&key.verifying_key(), bytes, &sig).expect("a real signature must verify");
    }

    #[test]
    fn verify_rejects_tampered_bytes() {
        let key = generate_keypair();
        let bytes = b"original bytes";
        let sig = sign(&key, bytes);
        let err = verify(&key.verifying_key(), b"tampered bytes!!", &sig).unwrap_err();
        assert!(matches!(err, RegistryError::BadSignature));
    }

    #[test]
    fn verify_rejects_a_signature_from_a_different_key() {
        let signer = generate_keypair();
        let attacker = generate_keypair();
        let bytes = b"catalog bytes";
        let sig = sign(&attacker, bytes);
        let err = verify(&signer.verifying_key(), bytes, &sig).unwrap_err();
        assert!(matches!(err, RegistryError::BadSignature));
    }

    #[test]
    fn verify_rejects_garbage_signature_text() {
        let key = generate_keypair();
        let err = verify(&key.verifying_key(), b"bytes", "not hex at all").unwrap_err();
        assert!(matches!(err, RegistryError::BadSignature));
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [7u8; 64];
        let encoded = hex::encode(bytes);
        assert_eq!(hex::decode(&encoded).unwrap(), bytes.to_vec());
    }
}
