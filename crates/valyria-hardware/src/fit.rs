//! Model/hardware compatibility scoring (§39: "model selection must use
//! measured capabilities"). [`ModelRequirement`] is a deliberately minimal
//! vocabulary type — the real model registry (layer 4, Phase 9) will carry
//! much richer metadata, but *needs this exact shape* to compute fit, so it
//! lives here rather than being duplicated once the registry exists.

use serde::{Deserialize, Serialize};

use crate::report::HardwareReport;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelRequirement {
    pub min_ram_bytes: u64,
    /// On a unified-memory system this is compared against available RAM,
    /// not a separate VRAM pool — see [`HardwareReport::unified_memory`].
    pub min_vram_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Fit {
    /// Comfortably fits with headroom to spare.
    Comfortable,
    /// Fits, but uses a large fraction of available resources —
    /// `est_util` is that fraction (0.0-1.0+; can exceed 1.0 slightly
    /// for the RAM-only-tight case reported alongside a hard VRAM miss).
    Tight {
        est_util: f64,
    },
    WillNotFit {
        reason: WillNotFitReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum WillNotFitReason {
    InsufficientRam,
    InsufficientVram,
}

/// Above this fraction of *available* (not total) memory, a fit is
/// reported as `Tight` rather than `Comfortable` — leaving headroom for
/// the OS, other loaded models, and the runtime's own working set (§41).
const TIGHT_THRESHOLD: f64 = 0.7;

pub fn fits(requirement: &ModelRequirement, hw: &HardwareReport) -> Fit {
    if requirement.min_ram_bytes > hw.ram_available_bytes {
        return Fit::WillNotFit {
            reason: WillNotFitReason::InsufficientRam,
        };
    }

    let vram_budget = if hw.unified_memory {
        Some(hw.ram_available_bytes)
    } else {
        hw.gpus.iter().filter_map(|g| g.vram_bytes).max()
    };

    if let Some(min_vram) = requirement.min_vram_bytes {
        match vram_budget {
            Some(budget) if min_vram <= budget => {}
            Some(_) => {
                return Fit::WillNotFit {
                    reason: WillNotFitReason::InsufficientVram,
                }
            }
            None => {
                return Fit::WillNotFit {
                    reason: WillNotFitReason::InsufficientVram,
                }
            }
        }
    }

    let ram_util = requirement.min_ram_bytes as f64 / hw.ram_available_bytes as f64;
    let vram_util = match (requirement.min_vram_bytes, vram_budget) {
        (Some(need), Some(budget)) if budget > 0 => need as f64 / budget as f64,
        _ => 0.0,
    };
    let est_util = ram_util.max(vram_util);

    if est_util > TIGHT_THRESHOLD {
        Fit::Tight { est_util }
    } else {
        Fit::Comfortable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{CpuInfo, DiskInfo, GpuInfo};

    fn hw(ram_available: u64, gpus: Vec<GpuInfo>, unified: bool) -> HardwareReport {
        HardwareReport {
            os: "test".into(),
            os_version: None,
            arch: "test".into(),
            cpu: CpuInfo {
                brand: "test".into(),
                physical_cores: 4,
                logical_cores: 8,
                arch: "test".into(),
            },
            ram_total_bytes: ram_available * 2,
            ram_available_bytes: ram_available,
            gpus,
            unified_memory: unified,
            accelerator_present: None,
            disk: DiskInfo {
                total_bytes: 0,
                available_bytes: 0,
            },
        }
    }

    #[test]
    fn comfortable_when_well_under_budget() {
        let hw = hw(16_000_000_000, vec![], false);
        let req = ModelRequirement {
            min_ram_bytes: 4_000_000_000,
            min_vram_bytes: None,
        };
        assert_eq!(fits(&req, &hw), Fit::Comfortable);
    }

    #[test]
    fn tight_when_near_available_ram() {
        let hw = hw(10_000_000_000, vec![], false);
        let req = ModelRequirement {
            min_ram_bytes: 8_000_000_000, // 80% of available
            min_vram_bytes: None,
        };
        assert!(matches!(fits(&req, &hw), Fit::Tight { .. }));
    }

    #[test]
    fn will_not_fit_when_ram_insufficient() {
        let hw = hw(4_000_000_000, vec![], false);
        let req = ModelRequirement {
            min_ram_bytes: 8_000_000_000,
            min_vram_bytes: None,
        };
        assert_eq!(
            fits(&req, &hw),
            Fit::WillNotFit {
                reason: WillNotFitReason::InsufficientRam
            }
        );
    }

    #[test]
    fn unified_memory_uses_ram_as_vram_budget() {
        let hw = hw(
            16_000_000_000,
            vec![GpuInfo {
                name: "Apple M4".into(),
                vendor: None,
                core_count: Some(10),
                vram_bytes: None, // unified: no separate pool reported
            }],
            true,
        );
        let req = ModelRequirement {
            min_ram_bytes: 4_000_000_000,
            min_vram_bytes: Some(4_000_000_000),
        };
        assert_eq!(fits(&req, &hw), Fit::Comfortable);
    }

    #[test]
    fn discrete_gpu_below_required_vram_will_not_fit() {
        let hw = hw(
            32_000_000_000,
            vec![GpuInfo {
                name: "discrete".into(),
                vendor: None,
                core_count: None,
                vram_bytes: Some(4_000_000_000),
            }],
            false,
        );
        let req = ModelRequirement {
            min_ram_bytes: 4_000_000_000,
            min_vram_bytes: Some(8_000_000_000),
        };
        assert_eq!(
            fits(&req, &hw),
            Fit::WillNotFit {
                reason: WillNotFitReason::InsufficientVram
            }
        );
    }

    #[test]
    fn requiring_vram_with_no_gpu_at_all_will_not_fit() {
        let hw = hw(32_000_000_000, vec![], false);
        let req = ModelRequirement {
            min_ram_bytes: 1_000_000_000,
            min_vram_bytes: Some(1_000_000_000),
        };
        assert_eq!(
            fits(&req, &hw),
            Fit::WillNotFit {
                reason: WillNotFitReason::InsufficientVram
            }
        );
    }

    #[test]
    fn uses_available_not_total_ram() {
        // Total is huge, but available is tiny — fit must be judged on
        // what's actually free right now, matching §39's "measured
        // capabilities" requirement.
        let mut hw = hw(1_000_000_000, vec![], false);
        hw.ram_total_bytes = 64_000_000_000;
        let req = ModelRequirement {
            min_ram_bytes: 4_000_000_000,
            min_vram_bytes: None,
        };
        assert_eq!(
            fits(&req, &hw),
            Fit::WillNotFit {
                reason: WillNotFitReason::InsufficientRam
            }
        );
    }
}
