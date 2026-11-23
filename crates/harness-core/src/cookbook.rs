use serde::{Deserialize, Serialize};
use sysinfo::System;

/// Hardware specification information and LLM sizing recommendations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub total_memory_gb: f64,
    pub available_memory_gb: f64,
    pub cpu_cores: usize,
    pub os_name: String,
    pub arch: String,
    pub is_apple_silicon: bool,
}

impl HardwareProfile {
    pub fn detect() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();

        let total_mem_bytes = sys.total_memory();
        let avail_mem_bytes = sys.available_memory();

        let total_gb = (total_mem_bytes as f64) / (1024.0 * 1024.0 * 1024.0);
        let avail_gb = (avail_mem_bytes as f64) / (1024.0 * 1024.0 * 1024.0);

        let cpu_cores = sys.cpus().len();
        let os_name = System::name().unwrap_or_else(|| std::env::consts::OS.to_string());
        let arch = std::env::consts::ARCH.to_string();

        let is_apple_silicon = cfg!(target_os = "macos") && (arch == "aarch64" || arch == "arm");

        Self {
            total_memory_gb: (total_gb * 10.0).round() / 10.0,
            available_memory_gb: (avail_gb * 10.0).round() / 10.0,
            cpu_cores,
            os_name,
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
        report.push_str("               🖥️  Dex-Harness Hardware Cookbook                  \n");
        report.push_str("=================================================================\n\n");

        report.push_str("### 📊 Host System Specifications:\n");
        report.push_str(&format!("- **OS & Architecture:** {} ({})\n", self.os_name, self.arch));
        report.push_str(&format!("- **CPU Logical Cores:** {}\n", self.cpu_cores));
        report.push_str(&format!(
            "- **Memory:** {:.1} GB Total ({:.1} GB Available)\n",
            self.total_memory_gb, self.available_memory_gb
        ));
        if self.is_apple_silicon {
            report.push_str("- **Hardware Note:** 🍎 Apple Silicon detected (Unified Memory Architecture enables GPU acceleration for large local LLMs)\n");
        }

        report.push_str("\n### 🎯 Recommended Local Models (Ollama / vLLM / llama.cpp):\n\n");
        for (idx, rec) in self.recommend_models().iter().enumerate() {
            report.push_str(&format!(
                "{}. **{}** - {}\n   - Context Window: {}\n   - Memory Budget: {}\n   - Note: {}\n\n",
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
        assert!(profile.cpu_cores > 0);
        let report = profile.generate_report();
        assert!(report.contains("Hardware Cookbook"));
        assert!(report.contains("Recommended Local Models"));
    }
}
