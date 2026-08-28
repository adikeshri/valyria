//! Post-install probe (§4.21): load the weights, run one generation,
//! exercise the tool-call transport ladder, and record measured tok/s and
//! memory. The real probe lives in the runtime adapters (Phase 9 scope
//! note: the llama.cpp / MLX adapters are the ones that can actually load a
//! GGUF); this crate defines the seam and a [`NullProber`] so the install
//! flow is testable end-to-end without a model runtime.

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use valyria_model_registry::{ModelCard, TransportPreference};

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    /// The model loaded and produced a non-empty completion.
    pub loads: bool,
    /// The transport that actually worked, which may be *lower* on the
    /// ladder than the catalog's `transport_preference`.
    pub working_transport: TransportPreference,
    /// Measured decode throughput, tokens/second.
    pub tokens_per_sec: f32,
    /// Resident memory attributable to the loaded model, bytes.
    pub measured_ram_bytes: u64,
}

#[async_trait]
pub trait Prober: Send + Sync {
    async fn probe(&self, weights: &Path, card: &ModelCard) -> Result<ProbeResult>;
}

/// Records the catalog's declared capabilities as if measured. Lets the
/// install flow complete and write a manifest without a real runtime.
#[derive(Debug, Default)]
pub struct NullProber;

#[async_trait]
impl Prober for NullProber {
    async fn probe(&self, _weights: &Path, card: &ModelCard) -> Result<ProbeResult> {
        Ok(ProbeResult {
            loads: true,
            working_transport: card.transport_preference,
            tokens_per_sec: 0.0,
            measured_ram_bytes: card.requirement.min_ram_bytes,
        })
    }
}
