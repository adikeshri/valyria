//! The hardware report shape (§39): what model selection and admission
//! control (§41) actually reason about.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuInfo {
    pub brand: String,
    pub physical_cores: usize,
    /// From `std::thread::available_parallelism()` — logical cores as the
    /// OS scheduler sees them, which is what actually bounds useful
    /// concurrency (§41 resource management reasons about this, not the
    /// physical count).
    pub logical_cores: usize,
    pub arch: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub vendor: Option<String>,
    pub core_count: Option<u32>,
    /// `None` on a unified-memory system (e.g. Apple Silicon) — there is
    /// no separate VRAM pool to report, which is itself meaningful
    /// information, not a missing value.
    pub vram_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DiskInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareReport {
    pub os: String,
    pub os_version: Option<String>,
    pub arch: String,
    pub cpu: CpuInfo,
    pub ram_total_bytes: u64,
    pub ram_available_bytes: u64,
    pub gpus: Vec<GpuInfo>,
    /// True on systems where CPU and GPU share one memory pool (Apple
    /// Silicon today) — model-fit scoring (§39) must use *available RAM*
    /// as the effective VRAM budget on these systems rather than treating
    /// "no dedicated VRAM" as "no GPU memory available".
    pub unified_memory: bool,
    /// Best-effort: is a dedicated ML accelerator present (e.g. Apple
    /// Neural Engine)? `None` means "not probed for on this platform yet",
    /// distinct from `Some(false)` meaning "probed, and absent".
    pub accelerator_present: Option<bool>,
    pub disk: DiskInfo,
}
