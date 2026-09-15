//! The catalog: an in-memory index of [`ModelCard`]s. Loads from the
//! compiled-in `catalog.json` by default ([`Catalog::embedded`]); a custom
//! JSON blob can be supplied for tests, or a signed remote refresh via
//! [`Catalog::verify_and_parse_signed`] (§ M6, [`crate::signing`]).

use serde::Deserialize;

use crate::card::ModelCard;
use crate::error::{RegistryError, Result};
use crate::role::ModelRole;
use crate::signing;

const EMBEDDED_JSON: &str = include_str!("catalog.json");

#[derive(Debug, Deserialize)]
struct CatalogFile {
    version: u32,
    models: Vec<ModelCard>,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    cards: Vec<ModelCard>,
    /// The source file's own monotonic counter — `0` for a catalog built
    /// via [`Self::from_cards`], which has no file to carry one.
    /// [`Self::verify_and_parse_signed`] uses this to refuse a validly-
    /// signed but stale (replayed) refresh.
    version: u32,
}

impl Catalog {
    /// The catalog compiled into the binary. Parsing is fallible only if
    /// the checked-in JSON is broken, which a unit test guards against, so
    /// this is safe to `expect` at startup.
    pub fn embedded() -> Result<Self> {
        Self::from_json(EMBEDDED_JSON)
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let file: CatalogFile =
            serde_json::from_str(json).map_err(|e| RegistryError::MalformedCatalog {
                detail: e.to_string(),
            })?;
        if let Some(dup) = first_duplicate_id(&file.models) {
            return Err(RegistryError::MalformedCatalog {
                detail: format!("duplicate model id {dup:?}"),
            });
        }
        Ok(Self {
            cards: file.models,
            version: file.version,
        })
    }

    /// Verify `signature_hex` over the exact `json_bytes` against
    /// `public_key`, then parse — in that order, so malformed-but-
    /// unsigned bytes never even reach the JSON parser. No staleness
    /// check: use this to re-load an *already-accepted* signed catalog
    /// (re-verified because trusting unauthenticated on-disk state is
    /// never free, but there is no "currently cached version" to replay
    /// against when the thing on disk *is* the cache). For accepting a
    /// *new* refresh, use [`Self::verify_and_parse_signed`] instead.
    pub fn verify_and_parse(
        json_bytes: &[u8],
        signature_hex: &str,
        public_key: &ed25519_dalek::VerifyingKey,
    ) -> Result<Self> {
        signing::verify(public_key, json_bytes, signature_hex)?;
        let json =
            std::str::from_utf8(json_bytes).map_err(|e| RegistryError::MalformedCatalog {
                detail: format!("not valid UTF-8: {e}"),
            })?;
        Self::from_json(json)
    }

    /// [`Self::verify_and_parse`], then refuse a validly-signed catalog
    /// whose `version` is not strictly greater than `min_version` (the
    /// currently-cached one): a legitimately signed but stale catalog is
    /// a replay/rollback, not a real refresh.
    pub fn verify_and_parse_signed(
        json_bytes: &[u8],
        signature_hex: &str,
        public_key: &ed25519_dalek::VerifyingKey,
        min_version: u32,
    ) -> Result<Self> {
        let catalog = Self::verify_and_parse(json_bytes, signature_hex, public_key)?;
        if catalog.version <= min_version {
            return Err(RegistryError::NotNewer {
                offered: catalog.version,
                current: min_version,
            });
        }
        Ok(catalog)
    }

    pub fn from_cards(cards: Vec<ModelCard>) -> Self {
        Self { cards, version: 0 }
    }

    /// The source file's own monotonic version counter (`0` for a catalog
    /// with no backing file, i.e. built via [`Self::from_cards`]).
    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn cards(&self) -> &[ModelCard] {
        &self.cards
    }

    pub fn get(&self, id: &str) -> Option<&ModelCard> {
        self.cards.iter().find(|c| c.id == id)
    }

    pub fn require(&self, id: &str) -> Result<&ModelCard> {
        self.get(id)
            .ok_or_else(|| RegistryError::UnknownModel { id: id.to_string() })
    }

    /// Every card the catalog lists as usable for `role` (suitability > 0),
    /// most-suitable first.
    pub fn candidates_for_role(&self, role: ModelRole) -> Vec<&ModelCard> {
        let mut out: Vec<&ModelCard> = self
            .cards
            .iter()
            .filter(|c| c.suitability(role) > 0)
            .collect();
        out.sort_by(|a, b| {
            b.suitability(role)
                .cmp(&a.suitability(role))
                .then(a.id.cmp(&b.id))
        });
        out
    }
}

fn first_duplicate_id(cards: &[ModelCard]) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    for c in cards {
        if !seen.insert(c.id.as_str()) {
            return Some(c.id.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses() {
        let catalog = Catalog::embedded().unwrap();
        assert!(!catalog.cards().is_empty());
    }

    #[test]
    fn embedded_catalog_has_a_model_for_every_role() {
        let catalog = Catalog::embedded().unwrap();
        for role in ModelRole::ALL {
            assert!(
                !catalog.candidates_for_role(role).is_empty(),
                "no candidate model for role {role}"
            );
        }
    }

    #[test]
    fn every_embedded_content_hash_is_64_hex() {
        // Every `EngineKind::LlamaCpp` card is independently verified by a
        // real blake3 hex digest of the downloaded file. `Mlx` cards carry
        // a documented sentinel instead (`valyria-model-store`'s
        // `MLX_LAZY_DOWNLOAD_SENTINEL`) — there is no single file *this*
        // catalog controls to hash, since `mlx_lm.server` resolves and
        // caches the weights itself from the repo id in `source_url`.
        let catalog = Catalog::embedded().unwrap();
        for c in catalog.cards() {
            if c.engine == crate::card::EngineKind::Mlx {
                continue;
            }
            assert_eq!(c.content_hash.len(), 64, "{}", c.id);
            assert!(
                c.content_hash.chars().all(|ch| ch.is_ascii_hexdigit()),
                "{}",
                c.id
            );
        }
    }

    #[test]
    fn candidates_are_sorted_by_suitability_desc() {
        let catalog = Catalog::embedded().unwrap();
        let cands = catalog.candidates_for_role(ModelRole::PrimaryCoder);
        for pair in cands.windows(2) {
            assert!(
                pair[0].suitability(ModelRole::PrimaryCoder)
                    >= pair[1].suitability(ModelRole::PrimaryCoder)
            );
        }
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let json = r#"{"version":1,"models":[
            {"id":"x","family":"f","display_name":"X","parameters_b":1.0,"quantization":"q4_k_m",
             "context_length":2048,"file_size_bytes":1,
             "recommended_sampling":{"temperature":0.2,"top_p":0.9,"max_tokens":null,"stop":[]},
             "requirement":{"min_ram_bytes":1,"min_vram_bytes":null},
             "transport_preference":"native","supports_native_tools":true,"supports_grammar":false,
             "source_url":"u","content_hash":"aa","license_name":"MIT"},
            {"id":"x","family":"f","display_name":"X2","parameters_b":1.0,"quantization":"q4_k_m",
             "context_length":2048,"file_size_bytes":1,
             "recommended_sampling":{"temperature":0.2,"top_p":0.9,"max_tokens":null,"stop":[]},
             "requirement":{"min_ram_bytes":1,"min_vram_bytes":null},
             "transport_preference":"native","supports_native_tools":true,"supports_grammar":false,
             "source_url":"u","content_hash":"bb","license_name":"MIT"}
        ]}"#;
        assert!(matches!(
            Catalog::from_json(json),
            Err(RegistryError::MalformedCatalog { .. })
        ));
    }

    #[test]
    fn require_reports_unknown_id() {
        let catalog = Catalog::embedded().unwrap();
        assert!(matches!(
            catalog.require("does-not-exist"),
            Err(RegistryError::UnknownModel { .. })
        ));
    }

    fn minimal_catalog_json(version: u32) -> String {
        format!(
            r#"{{"version":{version},"models":[
            {{"id":"x","family":"f","display_name":"X","parameters_b":1.0,"quantization":"q4_k_m",
             "context_length":2048,"file_size_bytes":1,
             "recommended_sampling":{{"temperature":0.2,"top_p":0.9,"max_tokens":null,"stop":[]}},
             "requirement":{{"min_ram_bytes":1,"min_vram_bytes":null}},
             "transport_preference":"native","supports_native_tools":true,"supports_grammar":false,
             "source_url":"u","content_hash":"aa","license_name":"MIT"}}
        ]}}"#
        )
    }

    #[test]
    fn embedded_catalog_carries_its_real_version() {
        // Guards the field wiring itself, independent of what the number
        // happens to be right now.
        let catalog = Catalog::embedded().unwrap();
        assert!(catalog.version() >= 1);
    }

    #[test]
    fn from_cards_has_version_zero() {
        assert_eq!(Catalog::from_cards(vec![]).version(), 0);
    }

    #[test]
    fn verify_and_parse_signed_accepts_a_genuinely_newer_signed_catalog() {
        let key = crate::signing::generate_keypair();
        let json = minimal_catalog_json(5);
        let sig = crate::signing::sign(&key, json.as_bytes());
        let catalog = Catalog::verify_and_parse_signed(
            json.as_bytes(),
            &sig,
            &key.verifying_key(),
            /* min_version */ 4,
        )
        .expect("newer, validly-signed catalog must be accepted");
        assert_eq!(catalog.version(), 5);
        assert!(catalog.get("x").is_some());
    }

    #[test]
    fn verify_and_parse_signed_rejects_a_bad_signature() {
        let key = crate::signing::generate_keypair();
        let attacker = crate::signing::generate_keypair();
        let json = minimal_catalog_json(5);
        // Signed by someone other than the trusted key.
        let sig = crate::signing::sign(&attacker, json.as_bytes());
        let err = Catalog::verify_and_parse_signed(json.as_bytes(), &sig, &key.verifying_key(), 0)
            .unwrap_err();
        assert!(matches!(err, RegistryError::BadSignature));
    }

    #[test]
    fn verify_and_parse_signed_rejects_a_validly_signed_but_stale_replay() {
        let key = crate::signing::generate_keypair();
        let json = minimal_catalog_json(3);
        let sig = crate::signing::sign(&key, json.as_bytes());
        // Already have version 5 cached; a validly-signed version 3 is a
        // rollback/replay, not a real refresh.
        let err = Catalog::verify_and_parse_signed(json.as_bytes(), &sig, &key.verifying_key(), 5)
            .unwrap_err();
        assert!(matches!(
            err,
            RegistryError::NotNewer {
                offered: 3,
                current: 5
            }
        ));
    }

    #[test]
    fn verify_and_parse_signed_rejects_equal_version_too() {
        // Strictly greater, not greater-or-equal: replaying the exact
        // same (validly signed) catalog again must not re-trigger
        // whatever a caller does on a successful refresh.
        let key = crate::signing::generate_keypair();
        let json = minimal_catalog_json(5);
        let sig = crate::signing::sign(&key, json.as_bytes());
        let err = Catalog::verify_and_parse_signed(json.as_bytes(), &sig, &key.verifying_key(), 5)
            .unwrap_err();
        assert!(matches!(err, RegistryError::NotNewer { .. }));
    }

    #[test]
    fn verify_and_parse_signed_never_parses_tampered_bytes_even_if_json_shaped() {
        let key = crate::signing::generate_keypair();
        let json = minimal_catalog_json(5);
        let sig = crate::signing::sign(&key, json.as_bytes());
        // Same signature, different (still validly-shaped) bytes.
        let tampered = minimal_catalog_json(999);
        let err =
            Catalog::verify_and_parse_signed(tampered.as_bytes(), &sig, &key.verifying_key(), 0)
                .unwrap_err();
        assert!(matches!(err, RegistryError::BadSignature));
    }
}
