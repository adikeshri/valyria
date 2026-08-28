//! The catalog entry shape (§4.21). One `ModelCard` is everything the
//! runtime needs to *decide about* a model — fit, license, role
//! suitability, where to download it from and how to verify it — without
//! having downloaded a byte.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use valyria_hardware::ModelRequirement;
use valyria_model::SamplingParams;

use crate::role::ModelRole;

/// Weight quantization. Governs file size and the RAM/VRAM the loaded model
/// needs — the catalog records a distinct card per quantization variant
/// because their [`ModelRequirement`]s differ materially.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantization {
    /// Full-precision or bf16/fp16 weights.
    F16,
    Q8_0,
    Q6K,
    Q5KM,
    Q4KM,
    Q4_0,
    Q3KM,
    /// An embedding model published without a quantized variant.
    None,
}

impl Quantization {
    pub fn as_str(&self) -> &'static str {
        match self {
            Quantization::F16 => "f16",
            Quantization::Q8_0 => "q8_0",
            Quantization::Q6K => "q6_k",
            Quantization::Q5KM => "q5_k_m",
            Quantization::Q4KM => "q4_k_m",
            Quantization::Q4_0 => "q4_0",
            Quantization::Q3KM => "q3_k_m",
            Quantization::None => "none",
        }
    }
}

/// Which tool-call transport (D5) this model is known to handle. The
/// orchestrator's transport ladder starts here and degrades on failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportPreference {
    /// Server exposes an OpenAI-style `tool_calls` array the model fills
    /// reliably.
    Native,
    /// Constrained decoding (GBNF / JSON-schema) is the reliable path.
    Grammar,
    /// Only free-text works; the tolerant fenced-JSON recovery parser must
    /// carry it.
    FencedText,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCard {
    /// Stable catalog id, also the on-disk directory name in the model
    /// store, e.g. `qwen2.5-coder-7b-instruct-q4_k_m`.
    pub id: String,
    /// Model family for role-default lookups, e.g. `qwen2.5-coder`.
    pub family: String,
    pub display_name: String,
    /// Parameter count in billions (`7.0`, `0.137`).
    pub parameters_b: f32,
    pub quantization: Quantization,
    /// Maximum context window the weights support.
    pub context_length: u32,
    /// Size of the weights file on disk / to download.
    pub file_size_bytes: u64,
    /// Jinja chat template, when the catalog overrides the one embedded in
    /// the GGUF metadata. `None` = use the file's own template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template: Option<String>,
    /// Sampling the model's authors recommend; the orchestrator uses this
    /// as the default unless a task overrides it.
    pub recommended_sampling: SamplingParams,
    /// `role -> suitability score (0..=100)`. A role absent from the map
    /// scores 0 and the model is never auto-selected for it.
    #[serde(default)]
    pub role_suitability: BTreeMap<ModelRole, u8>,
    /// Memory needed once loaded — compared against measured hardware by
    /// `valyria_hardware::fits`.
    pub requirement: ModelRequirement,
    pub transport_preference: TransportPreference,
    pub supports_native_tools: bool,
    pub supports_grammar: bool,
    /// Where `valyria-model-store` downloads the weights from.
    pub source_url: String,
    /// blake3 hex of the complete weights file — the whole-file integrity
    /// check after download (§4.21 "never partial-on-success").
    pub content_hash: String,
    /// SPDX-ish identifier surfaced at install time (§4.21 license
    /// surfacing), e.g. `Apache-2.0`, `Llama-3.1-Community`.
    pub license_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_url: Option<String>,
}

impl ModelCard {
    /// Suitability of this model for `role`, `0` if the catalog does not
    /// list the pair.
    pub fn suitability(&self, role: ModelRole) -> u8 {
        self.role_suitability.get(&role).copied().unwrap_or(0)
    }
}
