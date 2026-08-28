//! `manifest.json` — the record written into a model's directory once its
//! download is verified. It is the source of truth for "what is installed"
//! on disk; the optional `installed_model` DB row (see [`crate::db`]) is a
//! fast index over these, rebuildable by rescanning the store.

use serde::{Deserialize, Serialize};
use valyria_model_registry::ModelCard;

use crate::probe::ProbeResult;

pub const MANIFEST_FILENAME: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// The catalog card this model was installed from, copied in full so
    /// the store is self-describing even if the catalog later changes.
    pub card: ModelCard,
    /// Weights filename inside the model directory (not a full path).
    pub weights_file: String,
    /// Bytes on disk of the weights file.
    pub size_bytes: u64,
    /// blake3 hex actually computed over the downloaded file — equals
    /// `card.content_hash` for a clean install; kept explicitly so
    /// `verify_integrity` compares measured-then vs measured-now.
    pub content_hash: String,
    pub installed_at_ms: i64,
    /// `None` if the model was installed with a `NullProber`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<ProbeResult>,
}

impl Manifest {
    pub fn license_name(&self) -> &str {
        &self.card.license_name
    }
}
