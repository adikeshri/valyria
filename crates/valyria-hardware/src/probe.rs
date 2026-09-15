//! Full hardware probe (§39), built on `sysinfo` for the
//! reliably-cross-platform parts (OS, CPU, RAM, disks) and the
//! best-effort platform-specific [`crate::gpu`] module for the rest.

use sysinfo::{Disks, System};

use crate::gpu::{is_apple_silicon, probe_gpus};
use crate::report::{CpuInfo, DiskInfo, HardwareReport};

pub fn probe() -> HardwareReport {
    let mut system = System::new_all();
    system.refresh_all();

    let cpu_brand = system
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let cpu = CpuInfo {
        brand: cpu_brand,
        physical_cores: system.physical_core_count().unwrap_or(0),
        logical_cores: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        arch: std::env::consts::ARCH.to_string(),
    };

    let disk = probe_primary_disk();
    let gpus = probe_gpus();
    let unified_memory = is_apple_silicon() && !gpus.is_empty();

    let ram_total_bytes = system.total_memory();
    let ram_available_bytes = available_memory_bytes(
        ram_total_bytes,
        system.available_memory(),
        system.used_memory(),
    );

    HardwareReport {
        os: std::env::consts::OS.to_string(),
        os_version: System::os_version(),
        arch: std::env::consts::ARCH.to_string(),
        cpu,
        ram_total_bytes,
        ram_available_bytes,
        gpus,
        unified_memory,
        accelerator_present: probe_accelerator(),
        disk,
    }
}

/// `sysinfo::System::available_memory()` has been observed returning `0`
/// on a real machine with real free RAM — a real macOS environment during
/// this crate's own development, `total_memory()`/`used_memory()` both
/// plausible at the same time. Whatever the root cause (a memory-pressure
/// API `sysinfo` can't read in some sandboxes or OS configurations), a
/// hard `0` here is worse than a slightly-conservative estimate: every
/// caller (`valyria_hardware::fits`, the M6 `ModelPool` budget) treats it
/// as "nothing fits anywhere, ever" — silently disabling role-binding
/// auto-derivation and model-pool admission rather than degrading
/// gracefully. `total - used` is a sound floor (it just doesn't credit
/// reclaimable cache pages, so it slightly *undercounts* what's really
/// free) and is never itself `0` unless the machine genuinely has none
/// left, so it's used whenever the reported figure is suspiciously `0`
/// on a machine that very much has RAM.
fn available_memory_bytes(ram_total_bytes: u64, reported: u64, used: u64) -> u64 {
    if reported == 0 && ram_total_bytes > 0 {
        ram_total_bytes.saturating_sub(used)
    } else {
        reported
    }
}

/// The disk backing the current working directory — the one that actually
/// matters for "is there room to install this model" (§40), rather than
/// summing every mounted volume, which can wildly overstate what's usable.
fn probe_primary_disk() -> DiskInfo {
    let disks = Disks::new_with_refreshed_list();
    let cwd = std::env::current_dir().unwrap_or_default();

    let best = disks
        .list()
        .iter()
        .filter(|d| cwd.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len()); // longest (most specific) matching mount

    match best {
        Some(d) => DiskInfo {
            total_bytes: d.total_space(),
            available_bytes: d.available_space(),
        },
        None => DiskInfo {
            total_bytes: 0,
            available_bytes: 0,
        },
    }
}

/// Best-effort accelerator detection. Today: the Apple Neural Engine is
/// present on every Apple Silicon chip, so that's a reliable positive
/// signal; every other platform reports "not probed" rather than a false
/// negative, since real detection (e.g. enumerating a discrete NPU) isn't
/// implemented yet.
fn probe_accelerator() -> Option<bool> {
    if is_apple_silicon() {
        Some(true)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_returns_plausible_values() {
        let report = probe();
        assert!(!report.os.is_empty());
        assert!(!report.arch.is_empty());
        assert!(report.cpu.logical_cores >= 1);
        assert!(
            report.ram_total_bytes > 0,
            "a real machine always has some RAM"
        );
        // Regression: `sysinfo::available_memory()` has been observed
        // returning 0 on a real machine with real RAM to spare — every
        // consumer of this field (role-binding auto-derivation, the M6
        // ModelPool budget) treats a 0 budget as "nothing ever fits",
        // silently disabling both features rather than degrading.
        assert!(
            report.ram_available_bytes > 0,
            "available RAM must never be reported as 0 on a real machine \
             (total={}, see available_memory_bytes's fallback)",
            report.ram_total_bytes
        );
        assert!(report.ram_available_bytes <= report.ram_total_bytes);
    }

    #[test]
    fn available_memory_falls_back_to_total_minus_used_when_reported_is_zero() {
        // The exact bug this guards: sysinfo's own `available_memory()`
        // (`reported`) came back 0 on a real 24GB machine with 18GB used
        // and total/used both clearly plausible.
        assert_eq!(
            available_memory_bytes(25_769_803_776, 0, 18_324_619_264),
            25_769_803_776 - 18_324_619_264,
        );
    }

    #[test]
    fn available_memory_passes_through_a_real_nonzero_report_unchanged() {
        assert_eq!(
            available_memory_bytes(16_000_000_000, 4_000_000_000, 12_000_000_000),
            4_000_000_000
        );
    }

    #[test]
    fn available_memory_with_no_ram_at_all_is_zero_fallback_or_not() {
        assert_eq!(available_memory_bytes(0, 0, 0), 0);
    }

    #[test]
    fn probe_never_panics_on_this_platform() {
        let _ = probe();
    }
}
