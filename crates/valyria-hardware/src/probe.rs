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

    HardwareReport {
        os: std::env::consts::OS.to_string(),
        os_version: System::os_version(),
        arch: std::env::consts::ARCH.to_string(),
        cpu,
        ram_total_bytes: system.total_memory(),
        ram_available_bytes: system.available_memory(),
        gpus,
        unified_memory,
        accelerator_present: probe_accelerator(),
        disk,
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
    }

    #[test]
    fn probe_never_panics_on_this_platform() {
        let _ = probe();
    }
}
