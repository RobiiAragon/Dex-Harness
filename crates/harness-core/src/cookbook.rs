use serde::{Deserialize, Serialize};
use sysinfo::System;

const BYTES_PER_GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Lightweight live memory usage snapshot (matches macOS Activity Monitor / Memory Cleaner).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemorySnapshot {
    pub total_memory_gb: f64,
    pub used_memory_gb: f64,
    pub available_memory_gb: f64,
    pub usage_percent: f64,
}

/// Hardware specification information and LLM sizing recommendations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub total_memory_gb: f64,
    #[serde(default)]
    pub used_memory_gb: f64,
    pub available_memory_gb: f64,
    #[serde(default)]
    pub usage_percent: f64,
    pub cpu_cores: usize,
    #[serde(default)]
    pub cpu_brand: String,
    pub os_name: String,
    #[serde(default)]
    pub os_version: String,
    pub arch: String,
    pub is_apple_silicon: bool,
}

/// Cleans raw `sysctl -n machdep.cpu.brand_string` output (e.g. `"Apple M5 Max\n"`).
pub fn parse_cpu_brand_string(raw: &str) -> Option<String> {
    let cleaned = raw.trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// Formats `sw_vers -productVersion` and optional `sw_vers -buildVersion` into a clean display label.
pub fn format_macos_version(product_version: &str, build_version: Option<&str>) -> String {
    let pv = product_version.trim();
    match build_version.map(str::trim).filter(|b| !b.is_empty()) {
        Some(b) if !pv.is_empty() => format!("macOS {pv} ({b})"),
        _ if !pv.is_empty() => format!("macOS {pv}"),
        _ => "macOS".to_string(),
    }
}

fn round1(val: f64) -> f64 {
    (val * 10.0).round() / 10.0
}

/// Parses macOS `vm_stat` output together with `hw.memsize` (`total_bytes`) to compute
/// Activity Monitor / Memory Cleaner–accurate memory usage:
/// - `used = (Active + Wired + Occupied by compressor) * page_size`
/// - `available = total - used` (includes Free + Inactive + Speculative + Purgeable pages)
pub fn parse_macos_vm_stat(vm_stat_output: &str, total_bytes: u64) -> Option<MemorySnapshot> {
    if total_bytes == 0 {
        return None;
    }

    let mut page_size: u64 = if cfg!(target_arch = "aarch64") {
        16384
    } else {
        4096
    };
    let mut pages_active: Option<u64> = None;
    let mut pages_wired: Option<u64> = None;
    let mut pages_compressed: Option<u64> = None;

    for line in vm_stat_output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("page size of") {
            if let Some(after) = trimmed.split("page size of").nth(1) {
                if let Some(num_token) = after.split_whitespace().next() {
                    if let Ok(parsed) = num_token.parse::<u64>() {
                        if parsed > 0 {
                            page_size = parsed;
                        }
                    }
                }
            }
            continue;
        }

        if let Some((label, value)) = trimmed.split_once(':') {
            let clean_val = value.trim().trim_end_matches('.');
            let parsed_pages = match clean_val.parse::<u64>() {
                Ok(v) => v,
                Err(_) => continue,
            };
            match label.trim() {
                "Pages active" => pages_active = Some(parsed_pages),
                "Pages wired down" => pages_wired = Some(parsed_pages),
                "Pages occupied by compressor" => pages_compressed = Some(parsed_pages),
                _ => {}
            }
        }
    }

    let active = pages_active?;
    let wired = pages_wired?;
    let compressed = pages_compressed.unwrap_or(0);

    let used_pages = active.saturating_add(wired).saturating_add(compressed);
    let used_bytes = used_pages.saturating_mul(page_size).min(total_bytes);
    let available_bytes = total_bytes.saturating_sub(used_bytes);

    let total_gb = (total_bytes as f64) / BYTES_PER_GIB;
    let used_gb = (used_bytes as f64) / BYTES_PER_GIB;
    let avail_gb = (available_bytes as f64) / BYTES_PER_GIB;
    let usage_pct = if total_bytes > 0 {
        (used_bytes as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };

    Some(MemorySnapshot {
        total_memory_gb: round1(total_gb),
        used_memory_gb: round1(used_gb),
        available_memory_gb: round1(avail_gb),
        usage_percent: round1(usage_pct),
    })
}

#[cfg(target_os = "macos")]
fn detect_macos_memory() -> Option<MemorySnapshot> {
    let memsize_out = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    if !memsize_out.status.success() {
        return None;
    }
    let total_bytes: u64 = String::from_utf8_lossy(&memsize_out.stdout)
        .trim()
        .parse()
        .ok()?;

    let vm_out = std::process::Command::new("vm_stat").output().ok()?;
    if !vm_out.status.success() {
        return None;
    }
    let vm_text = String::from_utf8_lossy(&vm_out.stdout);
    parse_macos_vm_stat(&vm_text, total_bytes)
}

fn detect_sysinfo_memory() -> MemorySnapshot {
    let mut sys = System::new();
    sys.refresh_memory();

    let total_bytes = sys.total_memory();
    let avail_bytes = sys.available_memory().min(total_bytes);
    let used_bytes = total_bytes.saturating_sub(avail_bytes);

    let total_gb = (total_bytes as f64) / BYTES_PER_GIB;
    let used_gb = (used_bytes as f64) / BYTES_PER_GIB;
    let avail_gb = (avail_bytes as f64) / BYTES_PER_GIB;
    let usage_pct = if total_bytes > 0 {
        (used_bytes as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };

    MemorySnapshot {
        total_memory_gb: round1(total_gb),
        used_memory_gb: round1(used_gb),
        available_memory_gb: round1(avail_gb),
        usage_percent: round1(usage_pct),
    }
}

#[cfg(target_os = "macos")]
fn detect_macos_cpu_brand() -> Option<String> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_cpu_brand_string(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(target_os = "macos")]
fn detect_macos_os_version() -> Option<String> {
    let ver = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    if !ver.status.success() {
        return None;
    }
    let pv = String::from_utf8_lossy(&ver.stdout).trim().to_string();
    let build = std::process::Command::new("sw_vers")
        .arg("-buildVersion")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    Some(format_macos_version(&pv, build.as_deref()))
}

impl HardwareProfile {
    /// Lightweight live memory probe suitable for periodic polling (e.g. every 3s).
    /// On macOS uses `sysctl -n hw.memsize` + `vm_stat` (Active + Wired + Compressed).
    pub fn detect_live_memory() -> MemorySnapshot {
        #[cfg(target_os = "macos")]
        if let Some(snap) = detect_macos_memory() {
            return snap;
        }
        detect_sysinfo_memory()
    }

    pub fn detect() -> Self {
        let mem = Self::detect_live_memory();
        let cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or_else(|_| {
                let mut sys = System::new();
                sys.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
                sys.cpus().len().max(1)
            });

        let os_name = System::name().unwrap_or_else(|| std::env::consts::OS.to_string());
        let arch = std::env::consts::ARCH.to_string();
        let is_apple_silicon = cfg!(target_os = "macos") && (arch == "aarch64" || arch == "arm");

        let cpu_brand = {
            #[cfg(target_os = "macos")]
            let mac_brand = detect_macos_cpu_brand();
            #[cfg(not(target_os = "macos"))]
            let mac_brand: Option<String> = None;

            mac_brand.unwrap_or_else(|| {
                let mut sys = System::new();
                sys.refresh_cpu_list(sysinfo::CpuRefreshKind::everything());
                sys.cpus()
                    .first()
                    .map(|c| c.brand().trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        if is_apple_silicon {
                            "Apple Silicon".to_string()
                        } else {
                            arch.clone()
                        }
                    })
            })
        };

        let os_version = {
            #[cfg(target_os = "macos")]
            let mac_ver = detect_macos_os_version();
            #[cfg(not(target_os = "macos"))]
            let mac_ver: Option<String> = None;

            mac_ver.unwrap_or_else(|| {
                System::long_os_version().unwrap_or_else(|| format!("{os_name} ({arch})"))
            })
        };

        Self {
            total_memory_gb: mem.total_memory_gb,
            used_memory_gb: mem.used_memory_gb,
            available_memory_gb: mem.available_memory_gb,
            usage_percent: mem.usage_percent,
            cpu_cores,
            cpu_brand,
            os_name,
            os_version,
            arch,
            is_apple_silicon,
        }
    }

    /// Generates tailored local model and context window recommendations.
    pub fn recommend_models(&self) -> Vec<ModelRecommendation> {
        let mut recs = Vec::new();
        let mem = self.total_memory_gb;

        if mem >= 60.0 {
            recs.push(ModelRecommendation {
                tier: "Frontier Local (Heavy)".to_string(),
                model_name: "Qwen2.5-Coder 32B / Llama-3.3 70B (q4_k_m)".to_string(),
                recommended_vram: "40-48 GB Unified RAM".to_string(),
                max_context_window: "64k - 128k tokens".to_string(),
                notes: "Superb reasoning and complex architecture refactoring".to_string(),
            });
        }

        if mem >= 28.0 {
            recs.push(ModelRecommendation {
                tier: "Performance Tier (Recommended)".to_string(),
                model_name: "Qwen2.5-Coder 32B (q4_k_m) / DeepSeek-R1-Distill 14B".to_string(),
                recommended_vram: "20-24 GB Unified RAM".to_string(),
                max_context_window: "32k - 64k tokens".to_string(),
                notes: "Exceptional code synthesis and agentic tool-use capability".to_string(),
            });
        }

        if mem >= 14.0 {
            recs.push(ModelRecommendation {
                tier: "Balanced Developer Tier".to_string(),
                model_name: "Qwen2.5-Coder 14B (q4_k_m) / Llama-3.1 8B (q8_0)".to_string(),
                recommended_vram: "10-12 GB RAM".to_string(),
                max_context_window: "16k - 32k tokens".to_string(),
                notes: "Fast inference speed with reliable tool calling and parsing".to_string(),
            });
        }

        recs.push(ModelRecommendation {
            tier: "Lightweight & Fast Tier".to_string(),
            model_name: "Qwen2.5-Coder 7B (q4_k_m) / Llama-3.2 3B".to_string(),
            recommended_vram: "6-8 GB RAM".to_string(),
            max_context_window: "8k - 16k tokens".to_string(),
            notes: "Ultra-low latency, ideal for quick edits and explorations".to_string(),
        });

        recs
    }

    /// Formats hardware profile and recommendations into a comprehensive report.
    pub fn generate_report(&self) -> String {
        let mut report = String::new();
        report.push_str("=================================================================\n");
        report.push_str("                  Dex-Harness Hardware Cookbook                  \n");
        report.push_str("=================================================================\n\n");

        report.push_str("### Host System Specifications\n");
        report.push_str(&format!("- Chip / Processor: {} ({} logical cores)\n", self.cpu_brand, self.cpu_cores));
        report.push_str(&format!("- OS & Architecture: {} ({})\n", self.os_version, self.arch));
        report.push_str(&format!(
            "- Memory: {:.1} GB Used / {:.1} GB Total ({:.1} GB Available · {:.1}%)\n",
            self.used_memory_gb, self.total_memory_gb, self.available_memory_gb, self.usage_percent
        ));
        if self.is_apple_silicon {
            report.push_str("- Hardware Note: Apple Silicon detected (Unified Memory Architecture enables Metal/MLX GPU acceleration for large local LLMs)\n");
        }

        report.push_str("\n### Recommended Local Models (Dex Native MLX / llama.cpp / Ollama / LM Studio)\n\n");
        for (idx, rec) in self.recommend_models().iter().enumerate() {
            report.push_str(&format!(
                "{}. {} — {}\n   - Context Window: {}\n   - Memory Budget: {}\n   - Note: {}\n\n",
                idx + 1,
                rec.tier,
                rec.model_name,
                rec.max_context_window,
                rec.recommended_vram,
                rec.notes
            ));
        }

        report
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRecommendation {
    pub tier: String,
    pub model_name: String,
    pub recommended_vram: String,
    pub max_context_window: String,
    pub notes: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hardware_profile_detection() {
        let profile = HardwareProfile::detect();
        assert!(profile.total_memory_gb > 0.0);
        assert!(profile.used_memory_gb >= 0.0);
        assert!(profile.available_memory_gb >= 0.0);
        assert!(profile.usage_percent >= 0.0 && profile.usage_percent <= 100.0);
        assert!(profile.cpu_cores > 0);
        let report = profile.generate_report();
        assert!(report.contains("Hardware Cookbook"));
        assert!(report.contains("Recommended Local Models"));
    }

    #[test]
    fn test_parse_macos_vm_stat_apple_silicon_48gb() {
        let sample = "\
Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                   543688.
Pages active:                                1013992.
Pages inactive:                              1009920.
Pages speculative:                             39672.
Pages throttled:                                   0.
Pages wired down:                             370849.
Pages purgeable:                               91882.
Pages stored in compressor:                   261718.
Pages occupied by compressor:                 106939.
";
        let total_48gb: u64 = 48 * 1024 * 1024 * 1024;
        let snap = parse_macos_vm_stat(sample, total_48gb).expect("should parse vm_stat");
        // (1013992 + 370849 + 106939) * 16384 / 1024^3 = 22.763 GB -> 22.8 GB
        // available = 48.0 - 22.763 = 25.237 GB -> 25.2 GB
        // usage = 47.4%
        assert_eq!(snap.total_memory_gb, 48.0);
        assert_eq!(snap.used_memory_gb, 22.8);
        assert_eq!(snap.available_memory_gb, 25.2);
        assert_eq!(snap.usage_percent, 47.4);
    }

    #[test]
    fn test_parse_macos_vm_stat_4k_pages_and_invalid() {
        let sample_4k = "\
Mach Virtual Memory Statistics: (page size of 4096 bytes)
Pages active:                                1048576.
Pages wired down:                             524288.
Pages occupied by compressor:                 524288.
";
        // (1048576 + 524288 + 524288) * 4096 = 8 GiB out of 16 GiB -> 50.0%
        let total_16gb: u64 = 16 * 1024 * 1024 * 1024;
        let snap = parse_macos_vm_stat(sample_4k, total_16gb).expect("should parse 4k pages");
        assert_eq!(snap.total_memory_gb, 16.0);
        assert_eq!(snap.used_memory_gb, 8.0);
        assert_eq!(snap.available_memory_gb, 8.0);
        assert_eq!(snap.usage_percent, 50.0);

        assert!(parse_macos_vm_stat("invalid garbage", total_16gb).is_none());
        assert!(parse_macos_vm_stat(sample_4k, 0).is_none());
    }

    #[test]
    fn test_parse_cpu_brand_and_macos_version() {
        assert_eq!(
            parse_cpu_brand_string("  Apple M5 Max\n"),
            Some("Apple M5 Max".to_string())
        );
        assert_eq!(parse_cpu_brand_string("   "), None);
        assert_eq!(
            format_macos_version("27.2.1", Some("26B131")),
            "macOS 27.2.1 (26B131)"
        );
        assert_eq!(format_macos_version("15.4", None), "macOS 15.4");
    }
}
