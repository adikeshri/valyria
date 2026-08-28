//! The catalog: an in-memory index of [`ModelCard`]s. Loads from the
//! compiled-in `catalog.json` by default ([`Catalog::embedded`]); a custom
//! JSON blob can be supplied for tests or a future signed remote refresh.

use serde::Deserialize;

use crate::card::ModelCard;
use crate::error::{RegistryError, Result};
use crate::role::ModelRole;

const EMBEDDED_JSON: &str = include_str!("catalog.json");

#[derive(Debug, Deserialize)]
struct CatalogFile {
    #[allow(dead_code)]
    version: u32,
    models: Vec<ModelCard>,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    cards: Vec<ModelCard>,
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
        Ok(Self { cards: file.models })
    }

    pub fn from_cards(cards: Vec<ModelCard>) -> Self {
        Self { cards }
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
        let catalog = Catalog::embedded().unwrap();
        for c in catalog.cards() {
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
}
