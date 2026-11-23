use harness_core::cookbook::HardwareProfile;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::ModelError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfModelSummary {
    pub id: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub downloads: Option<u64>,
    #[serde(default)]
    pub likes: Option<u64>,
    #[serde(default, rename = "lastModified")]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryFit {
    Fits,
    Tight,
    TooLarge,
}

impl std::fmt::Display for MemoryFit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fits => write!(f, "Fits"),
            Self::Tight => write!(f, "Tight"),
            Self::TooLarge => write!(f, "Too Large"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfGgufFile {
    pub filename: String,
    pub size_bytes: u64,
    pub quantization: String,
    pub download_url: String,
    pub memory_fit: MemoryFit,
    pub ram_required_gb: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfModelDetails {
    pub id: String,
    pub files: Vec<HfGgufFile>,
}

#[derive(Debug, Deserialize)]
struct HfTreeItem {
    pub r#type: String,
    pub path: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Clone)]
pub struct HfClient {
    client: reqwest::Client,
    token: Option<String>,
}

impl Default for HfClient {
    fn default() -> Self {
        Self::new(None)
    }
}

impl HfClient {
    pub fn new(token: Option<String>) -> Self {
        let active_token = token.or_else(|| std::env::var("HF_TOKEN").ok());
        Self {
            client: reqwest::Client::builder()
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            token: active_token,
        }
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("Dex-Harness/0.5.0"));
        if let Some(ref token) = self.token {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", token)) {
                headers.insert(AUTHORIZATION, val);
            }
        }
        headers
    }

    pub async fn search_models(&self, query: &str, limit: usize) -> Result<Vec<HfModelSummary>, ModelError> {
        let url = format!(
            "https://huggingface.co/api/models?search={}&filter=gguf&sort=downloads&direction=-1&limit={}",
            urlencoding(query),
            limit
        );

        debug!(url = %url, "Searching Hugging Face models");
        let resp = self.client
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .map_err(|e| ModelError::Http {
                url: url.clone(),
                source: e,
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ModelError::HuggingFace {
                url,
                message: format!("HTTP {} - {}", status, body),
            });
        }

        let models = resp.json::<Vec<HfModelSummary>>().await.map_err(|e| ModelError::Http {
            url,
            source: e,
        })?;

        Ok(models)
    }

    pub async fn inspect_gguf_files(
        &self,
        repo_id: &str,
        profile: &HardwareProfile,
    ) -> Result<Vec<HfGgufFile>, ModelError> {
        let url = format!("https://huggingface.co/api/models/{}/tree/main", repo_id);

        debug!(repo_id = %repo_id, url = %url, "Inspecting repository files");
        let resp = self.client
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .map_err(|e| ModelError::Http {
                url: url.clone(),
                source: e,
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ModelError::HuggingFace {
                url,
                message: format!("HTTP {} - {}", status, body),
            });
        }

        let items = resp.json::<Vec<HfTreeItem>>().await.map_err(|e| ModelError::Http {
            url: url.clone(),
            source: e,
        })?;

        let mut files = Vec::new();

        for item in items {
            if item.r#type == "file" && item.path.to_lowercase().ends_with(".gguf") {
                let size_bytes = item.size.unwrap_or(0);
                let filename = item.path.split('/').next_back().unwrap_or(&item.path).to_string();
                let quantization = parse_quantization(&filename);
                let (memory_fit, ram_required_gb) = evaluate_memory_fit(size_bytes, profile);
                let download_url = format!(
                    "https://huggingface.co/{}/resolve/main/{}",
                    repo_id, item.path
                );

                files.push(HfGgufFile {
                    filename,
                    size_bytes,
                    quantization,
                    download_url,
                    memory_fit,
                    ram_required_gb,
                });
            }
        }

        // Sort files: prefer recommended Q4_K_M / Q5_K_M first, then by size ascending
        files.sort_by(|a, b| {
            let a_pref = preference_score(&a.quantization);
            let b_pref = preference_score(&b.quantization);
            b_pref.cmp(&a_pref).then_with(|| a.size_bytes.cmp(&b.size_bytes))
        });

        Ok(files)
    }
}

pub fn parse_quantization(filename: &str) -> String {
    let upper = filename.to_uppercase();

    // Standard list of GGUF quantizations checked in order of specificity
    let quant_patterns = [
        "Q4_K_M", "Q4_K_S", "Q5_K_M", "Q5_K_S", "Q3_K_L", "Q3_K_M", "Q3_K_S", "Q2_K",
        "Q6_K", "Q8_0", "Q8_1", "Q5_0", "Q5_1", "Q4_0", "Q4_1",
        "IQ4_XS", "IQ4_NL", "IQ3_M", "IQ3_S", "IQ2_M", "IQ2_S", "IQ1_S", "IQ1_M",
        "BF16", "F16", "F32",
    ];

    for pattern in quant_patterns {
        if upper.contains(pattern) {
            return pattern.to_string();
        }
    }

    "UNKNOWN".to_string()
}

pub fn evaluate_memory_fit(size_bytes: u64, profile: &HardwareProfile) -> (MemoryFit, f64) {
    let size_gb = size_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    // Overhead heuristic: ~20% additional memory required for KV cache and context buffers
    let ram_required_gb = (size_gb * 1.2 * 10.0).round() / 10.0;

    let fit = if ram_required_gb <= profile.available_memory_gb {
        MemoryFit::Fits
    } else if ram_required_gb <= profile.total_memory_gb * 0.95 {
        MemoryFit::Tight
    } else {
        MemoryFit::TooLarge
    };

    (fit, ram_required_gb)
}

fn preference_score(quant: &str) -> i32 {
    match quant {
        "Q4_K_M" => 10,
        "Q5_K_M" => 9,
        "Q4_K_S" => 8,
        "Q8_0" => 7,
        "Q6_K" => 6,
        "Q5_K_S" => 5,
        "Q4_0" => 4,
        "Q3_K_M" => 3,
        _ => 1,
    }
}

fn urlencoding(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.' || byte == b'~' {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{:02X}", byte));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_quantization_various_patterns() {
        assert_eq!(parse_quantization("qwen2.5-coder-7b-instruct-q4_k_m.gguf"), "Q4_K_M");
        assert_eq!(parse_quantization("Meta-Llama-3.1-8B-Instruct-Q8_0.gguf"), "Q8_0");
        assert_eq!(parse_quantization("deepseek-r1-distill-14b.Q5_K_M.gguf"), "Q5_K_M");
        assert_eq!(parse_quantization("phi-3-mini-iq4_xs.gguf"), "IQ4_XS");
        assert_eq!(parse_quantization("gemma-2-9b-it-bf16.gguf"), "BF16");
        assert_eq!(parse_quantization("custom-model.gguf"), "UNKNOWN");
    }

    #[test]
    fn test_evaluate_memory_fit() {
        let profile = HardwareProfile {
            total_memory_gb: 32.0,
            available_memory_gb: 16.0,
            cpu_cores: 10,
            os_name: "macOS".to_string(),
            arch: "aarch64".to_string(),
            is_apple_silicon: true,
        };

        // 4 GB model * 1.2 = 4.8 GB <= 16 GB available => Fits
        let (fit1, req1) = evaluate_memory_fit(4 * 1024 * 1024 * 1024, &profile);
        assert_eq!(fit1, MemoryFit::Fits);
        assert_eq!(req1, 4.8);

        // 18 GB model * 1.2 = 21.6 GB > 16 available but <= 32 total * 0.95 (30.4) => Tight
        let (fit2, req2) = evaluate_memory_fit(18 * 1024 * 1024 * 1024, &profile);
        assert_eq!(fit2, MemoryFit::Tight);
        assert_eq!(req2, 21.6);

        // 40 GB model * 1.2 = 48.0 GB > 30.4 GB total => TooLarge
        let (fit3, req3) = evaluate_memory_fit(40 * 1024 * 1024 * 1024, &profile);
        assert_eq!(fit3, MemoryFit::TooLarge);
        assert_eq!(req3, 48.0);
    }
}
