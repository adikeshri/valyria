//! `valyria-hardware` — layer 1 (Platform).
//!
//! Hardware detection (§39) and model/hardware fit scoring (§41). GPU/VRAM
//! detection is honestly best-effort and platform-specific — see
//! [`gpu`]'s module docs for exactly what is and isn't implemented today.

#![forbid(unsafe_code)]

pub mod cache;
pub mod fit;
pub mod gpu;
pub mod probe;
pub mod report;

pub use cache::CachedProbe;
pub use fit::{fits, Fit, ModelRequirement, WillNotFitReason};
pub use probe::probe;
pub use report::{CpuInfo, DiskInfo, GpuInfo, HardwareReport};
