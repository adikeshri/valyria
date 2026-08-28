//! Model selection (§39, §41): score `(model, role)` pairs against
//! **measured** hardware and pick a binding with an ordered fallback chain.
//!
//! Fit is delegated wholesale to `valyria_hardware::fits` so there is one
//! definition of "will this run" in the workspace; this module only layers
//! role suitability and a `Tight`-fit penalty on top.

use serde::{Deserialize, Serialize};
use valyria_hardware::{fits, Fit, HardwareReport};

use crate::card::ModelCard;
use crate::catalog::Catalog;
use crate::error::{RegistryError, Result};
use crate::role::ModelRole;

/// A model's score for one role on one machine. Higher is better; a card
/// that will not fit does not get a `CardScore` at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CardScore {
    /// Catalog suitability, `0..=100`.
    pub suitability: u8,
    pub fit: Fit,
    /// `suitability` minus a penalty for a `Tight` fit — the value
    /// [`select_for_role`] actually ranks on.
    pub adjusted: f64,
}

/// `None` when the card will not fit `hw`. A `Tight` fit is penalised in
/// proportion to how tight it is, so a comfortably-fitting slightly-less-
/// suitable model can legitimately win.
pub fn score_card_for_role(
    card: &ModelCard,
    role: ModelRole,
    hw: &HardwareReport,
) -> Option<CardScore> {
    let suitability = card.suitability(role);
    if suitability == 0 {
        return None;
    }
    let fit = fits(&card.requirement, hw);
    let penalty = match fit {
        Fit::Comfortable => 0.0,
        // est_util is 0.7..~1.0 in the Tight band; map that to a 0..~30
        // point penalty.
        Fit::Tight { est_util } => ((est_util - 0.7).max(0.0) * 100.0).min(30.0),
        Fit::WillNotFit { .. } => return None,
    };
    Some(CardScore {
        suitability,
        fit,
        adjusted: suitability as f64 - penalty,
    })
}

/// The best-fitting model for `role`, restricted to the ids in
/// `available` (what `valyria-model-store` reports installed). Pass an
/// empty slice to consider the whole catalog (planning "what would I need
/// to install").
pub fn select_for_role<'a>(
    catalog: &'a Catalog,
    role: ModelRole,
    hw: &HardwareReport,
    available: &[String],
) -> Result<&'a ModelCard> {
    let consider_all = available.is_empty();
    let mut best: Option<(&ModelCard, CardScore)> = None;
    for card in catalog.candidates_for_role(role) {
        if !consider_all && !available.iter().any(|id| id == &card.id) {
            continue;
        }
        let Some(score) = score_card_for_role(card, role, hw) else {
            continue;
        };
        let better = match &best {
            None => true,
            Some((_, b)) => {
                score.adjusted > b.adjusted
                    || (score.adjusted == b.adjusted && score.suitability > b.suitability)
            }
        };
        if better {
            best = Some((card, score));
        }
    }
    best.map(|(c, _)| c).ok_or(RegistryError::NoSuitableModel {
        role: role.to_string(),
    })
}

/// A concrete role → model decision plus the fit it was made under, so a
/// `Tight` binding can be surfaced to the user (§4.21 "requires
/// acknowledgement").
#[derive(Debug, Clone, PartialEq)]
pub struct RoleAssignment {
    pub role: ModelRole,
    pub card_id: String,
    pub fit: Fit,
}

/// A role's model and its ordered fallbacks (§38). `generate` tries
/// `primary`, then each fallback in turn on `Unavailable` / malformed
/// output, before giving up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleBinding {
    pub role: ModelRole,
    pub primary: String,
    #[serde(default)]
    pub fallbacks: Vec<String>,
}

impl RoleBinding {
    pub fn new(role: ModelRole, primary: impl Into<String>) -> Self {
        Self {
            role,
            primary: primary.into(),
            fallbacks: Vec::new(),
        }
    }

    pub fn with_fallback(mut self, id: impl Into<String>) -> Self {
        self.fallbacks.push(id.into());
        self
    }

    /// primary first, then fallbacks, in try order.
    pub fn chain(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.primary.as_str()).chain(self.fallbacks.iter().map(String::as_str))
    }

    /// Build a binding for `role` by picking the best installed model as
    /// primary and every other fitting installed candidate as a fallback,
    /// ordered by suitability.
    pub fn derive(
        catalog: &Catalog,
        role: ModelRole,
        hw: &HardwareReport,
        available: &[String],
    ) -> Result<Self> {
        let primary = select_for_role(catalog, role, hw, available)?;
        let mut fallbacks: Vec<String> = catalog
            .candidates_for_role(role)
            .into_iter()
            .filter(|c| c.id != primary.id)
            .filter(|c| available.is_empty() || available.iter().any(|id| id == &c.id))
            .filter(|c| score_card_for_role(c, role, hw).is_some())
            .map(|c| c.id.clone())
            .collect();
        fallbacks.dedup();
        Ok(Self {
            role,
            primary: primary.id.clone(),
            fallbacks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_hardware::report::{CpuInfo, DiskInfo, GpuInfo, HardwareReport};

    fn hw(ram_available: u64, unified: bool, vram: Option<u64>) -> HardwareReport {
        HardwareReport {
            os: "test".into(),
            os_version: None,
            arch: "test".into(),
            cpu: CpuInfo {
                brand: "test".into(),
                physical_cores: 8,
                logical_cores: 16,
                arch: "test".into(),
            },
            ram_total_bytes: ram_available * 2,
            ram_available_bytes: ram_available,
            gpus: vram
                .map(|v| {
                    vec![GpuInfo {
                        name: "gpu".into(),
                        vendor: None,
                        core_count: None,
                        vram_bytes: Some(v),
                    }]
                })
                .unwrap_or_default(),
            unified_memory: unified,
            accelerator_present: None,
            disk: DiskInfo {
                total_bytes: 0,
                available_bytes: 0,
            },
        }
    }

    #[test]
    fn big_machine_picks_the_most_suitable_coder() {
        let catalog = Catalog::embedded().unwrap();
        let hw = hw(64_000_000_000, true, None);
        let pick = select_for_role(&catalog, ModelRole::PrimaryCoder, &hw, &[]).unwrap();
        assert_eq!(pick.id, "qwen2.5-coder-7b-instruct-q4_k_m");
    }

    #[test]
    fn tiny_machine_cannot_fit_the_7b_and_gets_no_suitable_coder() {
        let catalog = Catalog::embedded().unwrap();
        // 3 GB available, unified: the 7B needs 7 GB RAM.
        let hw = hw(3_000_000_000, true, None);
        let err = select_for_role(&catalog, ModelRole::PrimaryCoder, &hw, &[]).unwrap_err();
        assert!(matches!(err, RegistryError::NoSuitableModel { .. }));
    }

    #[test]
    fn selection_is_restricted_to_available_ids() {
        let catalog = Catalog::embedded().unwrap();
        let hw = hw(64_000_000_000, true, None);
        let available = vec!["llama-3.1-8b-instruct-q4_k_m".to_string()];
        // llama scores lower than qwen-coder for PrimaryCoder, but it's
        // the only one installed.
        let pick = select_for_role(&catalog, ModelRole::PrimaryCoder, &hw, &available).unwrap();
        assert_eq!(pick.id, "llama-3.1-8b-instruct-q4_k_m");
    }

    #[test]
    fn tight_fit_is_penalised_below_a_comfortable_alternative() {
        // Two synthetic cards for the same role: A is more suitable but
        // only Tight; B is less suitable but Comfortable. B should win.
        let mut a = catalog_card("a", 90, 9_500_000_000);
        let mut b = catalog_card("b", 80, 2_000_000_000);
        a.family = "fam".into();
        b.family = "fam".into();
        let catalog = Catalog::from_cards(vec![a, b]);
        let hw = hw(10_000_000_000, true, None);
        let pick = select_for_role(&catalog, ModelRole::PrimaryCoder, &hw, &[]).unwrap();
        assert_eq!(pick.id, "b");
    }

    #[test]
    fn derive_binding_puts_best_first_and_others_as_fallbacks() {
        let catalog = Catalog::embedded().unwrap();
        let hw = hw(64_000_000_000, true, None);
        let binding = RoleBinding::derive(&catalog, ModelRole::PrimaryCoder, &hw, &[]).unwrap();
        assert_eq!(binding.primary, "qwen2.5-coder-7b-instruct-q4_k_m");
        assert!(!binding.fallbacks.is_empty());
        assert!(!binding.fallbacks.contains(&binding.primary));
        // chain() yields primary first.
        assert_eq!(
            binding.chain().next().unwrap(),
            "qwen2.5-coder-7b-instruct-q4_k_m"
        );
    }

    fn catalog_card(id: &str, suitability: u8, min_ram: u64) -> ModelCard {
        use crate::card::{Quantization, TransportPreference};
        use std::collections::BTreeMap;
        use valyria_hardware::ModelRequirement;
        use valyria_model::SamplingParams;

        let mut role_suitability = BTreeMap::new();
        role_suitability.insert(ModelRole::PrimaryCoder, suitability);
        ModelCard {
            id: id.into(),
            family: id.into(),
            display_name: id.into(),
            parameters_b: 7.0,
            quantization: Quantization::Q4KM,
            context_length: 4096,
            file_size_bytes: min_ram,
            chat_template: None,
            recommended_sampling: SamplingParams::default(),
            role_suitability,
            requirement: ModelRequirement {
                min_ram_bytes: min_ram,
                min_vram_bytes: None,
            },
            transport_preference: TransportPreference::Native,
            supports_native_tools: true,
            supports_grammar: true,
            source_url: "u".into(),
            content_hash: "00".repeat(32),
            license_name: "MIT".into(),
            license_url: None,
        }
    }
}
