//! GPU/accelerator probing. There is no reliable cross-platform crate for
//! this, so it's genuinely best-effort per platform: macOS shells out to
//! `system_profiler` (real, parsed against actual observed output — see
//! module tests), and other platforms honestly report an empty GPU list
//! rather than fabricate data. Extending this to CUDA/ROCm enumeration on
//! Linux and DXGI on Windows is future work (tracked, not faked).

use crate::report::GpuInfo;

pub fn probe_gpus() -> Vec<GpuInfo> {
    #[cfg(target_os = "macos")]
    {
        macos::probe_gpus()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Best-effort: is this an Apple Silicon Mac? Used both to fill
/// `unified_memory` and as one heuristic input to accelerator detection
/// (the Apple Neural Engine is present on every Apple Silicon chip).
pub fn is_apple_silicon() -> bool {
    cfg!(target_os = "macos") && std::env::consts::ARCH == "aarch64"
}

#[cfg(target_os = "macos")]
mod macos {
    use super::GpuInfo;

    pub fn probe_gpus() -> Vec<GpuInfo> {
        let Ok(output) = std::process::Command::new("system_profiler")
            .args(["SPDisplaysDataType", "-json"])
            .output()
        else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
            return Vec::new();
        };
        parse_sp_displays(&value)
    }

    /// Parses the `SPDisplaysDataType -json` shape. Defensive throughout:
    /// system_profiler's schema is not stable across macOS versions or
    /// hardware, so every field is optional and a missing/differently-typed
    /// field degrades to `None` rather than failing the whole probe.
    fn parse_sp_displays(root: &serde_json::Value) -> Vec<GpuInfo> {
        let Some(entries) = root.get("SPDisplaysDataType").and_then(|v| v.as_array()) else {
            return Vec::new();
        };

        entries
            .iter()
            .map(|entry| {
                let name = entry
                    .get("sppci_model")
                    .or_else(|| entry.get("_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown GPU")
                    .to_string();

                let vendor = entry
                    .get("spdisplays_vendor")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let core_count = entry
                    .get("sppci_cores")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<u32>().ok());

                // Different macOS versions have used different keys for
                // dedicated VRAM on Intel Macs with discrete GPUs; Apple
                // Silicon reports none of these, which correctly yields
                // `vram_bytes: None` (unified memory, no separate pool).
                let vram_bytes = [
                    "spdisplays_vram",
                    "spdisplays_vram_shared",
                    "spdisplays_vram_total",
                ]
                .iter()
                .find_map(|key| entry.get(*key).and_then(|v| v.as_str()))
                .and_then(parse_size_string);

                GpuInfo {
                    name,
                    vendor,
                    core_count,
                    vram_bytes,
                }
            })
            .collect()
    }

    /// Parses strings like `"8 GB"` or `"1536 MB"` into bytes.
    fn parse_size_string(s: &str) -> Option<u64> {
        let s = s.trim();
        let (number_part, unit_part) = s.split_once(' ')?;
        let number: f64 = number_part.parse().ok()?;
        let multiplier: u64 = match unit_part.to_uppercase().as_str() {
            "GB" => 1_000_000_000,
            "MB" => 1_000_000,
            "KB" => 1_000,
            "TB" => 1_000_000_000_000,
            _ => return None,
        };
        Some((number * multiplier as f64) as u64)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_real_apple_silicon_output_shape() {
            // Captured verbatim from `system_profiler SPDisplaysDataType
            // -json` on an Apple M4 machine — this is what "defensive
            // parsing" is being tested against, not a guess at the schema.
            let json = serde_json::json!({
                "SPDisplaysDataType": [
                    {
                        "_name": "Apple M4",
                        "spdisplays_vendor": "sppci_vendor_Apple",
                        "sppci_bus": "spdisplays_builtin",
                        "sppci_cores": "10",
                        "sppci_device_type": "spdisplays_gpu",
                        "sppci_model": "Apple M4"
                    }
                ]
            });

            let gpus = parse_sp_displays(&json);
            assert_eq!(gpus.len(), 1);
            assert_eq!(gpus[0].name, "Apple M4");
            assert_eq!(gpus[0].vendor.as_deref(), Some("sppci_vendor_Apple"));
            assert_eq!(gpus[0].core_count, Some(10));
            assert_eq!(
                gpus[0].vram_bytes, None,
                "unified memory: no separate VRAM pool"
            );
        }

        #[test]
        fn parses_a_hypothetical_discrete_gpu_with_vram() {
            let json = serde_json::json!({
                "SPDisplaysDataType": [
                    {
                        "sppci_model": "AMD Radeon Pro 5500M",
                        "spdisplays_vram_shared": "8 GB"
                    }
                ]
            });
            let gpus = parse_sp_displays(&json);
            assert_eq!(gpus[0].vram_bytes, Some(8_000_000_000));
        }

        #[test]
        fn missing_top_level_key_yields_empty_not_error() {
            let json = serde_json::json!({ "SomeOtherType": [] });
            assert!(parse_sp_displays(&json).is_empty());
        }

        #[test]
        fn size_string_parsing() {
            assert_eq!(parse_size_string("8 GB"), Some(8_000_000_000));
            assert_eq!(parse_size_string("1536 MB"), Some(1_536_000_000));
            assert_eq!(parse_size_string("garbage"), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_gpus_never_panics() {
        // Whatever platform this runs on, probing must degrade gracefully.
        let _ = probe_gpus();
    }
}
