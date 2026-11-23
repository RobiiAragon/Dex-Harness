use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;
use futures::future::join_all;
use harness_core::cookbook::HardwareProfile;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::debug;

use crate::error::ModelError;
use crate::scanner::infer_capabilities;

const HF_HTTP_TIMEOUT_SECS: u64 = 20;
const HF_AVATAR_TIMEOUT_MS: u64 = 2200;
const MAX_README_BYTES: usize = 128 * 1024;

static AVATAR_CACHE: OnceLock<RwLock<HashMap<String, Option<String>>>> = OnceLock::new();

fn avatar_cache() -> &'static RwLock<HashMap<String, Option<String>>> {
    AVATAR_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

#[derive(Debug, Deserialize)]
struct HfAvatarResponse {
    #[serde(rename = "avatarUrl")]
    avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfModelSummary {
    pub id: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub downloads: Option<u64>,
    #[serde(default)]
    pub likes: Option<u64>,
    #[serde(default, rename = "lastModified")]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub pipeline_tag: Option<String>,
    #[serde(default)]
    pub formats: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
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
    #[serde(default = "default_gguf_format")]
    pub format: String,
    #[serde(default)]
    pub is_recommended: bool,
    #[serde(default)]
    pub is_auxiliary: bool,
}

/// Returns true if a GGUF/model path represents an auxiliary weight (vision projector, MTP head,
/// draft model, or imatrix calibration file) that must never be auto-selected as `Recommended`.
pub fn is_auxiliary_model_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    let leaf = lower.rsplit('/').next().unwrap_or(&lower);
    leaf.contains("mmproj")
        || leaf.contains("imatrix")
        || leaf.starts_with("mtp-")
        || leaf.starts_with("mtp_")
        || leaf.contains("-mtp-")
        || leaf.contains("_mtp_")
        || leaf.contains("-mtp.")
        || leaf.contains("_mtp.")
        || leaf.contains("draft")
        || lower.contains("/mtp/")
        || lower.contains("/draft/")
}

fn default_gguf_format() -> String {
    "GGUF".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfModelDetails {
    pub id: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
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
                .timeout(Duration::from_secs(HF_HTTP_TIMEOUT_SECS))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            token: active_token,
        }
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("Dex-Harness/0.6.0"));
        if let Some(ref token) = self.token {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", token)) {
                headers.insert(AUTHORIZATION, val);
            }
        }
        headers
    }

    pub async fn search_models(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<HfModelSummary>, ModelError> {
        self.search_models_filtered(query, Some("gguf"), Some("downloads"), limit)
            .await
    }

    /// Searches Hugging Face Hub with optional format filter (`all`, `gguf`, `mlx`)
    /// and sort order (`downloads`, `likes`, `recent`/`lastModified`).
    pub async fn search_models_filtered(
        &self,
        query: &str,
        format: Option<&str>,
        sort: Option<&str>,
        limit: usize,
    ) -> Result<Vec<HfModelSummary>, ModelError> {
        let sort_param = match sort.unwrap_or("downloads").to_lowercase().as_str() {
            "likes" => "likes",
            "recent" | "lastmodified" | "last_modified" => "lastModified",
            _ => "downloads",
        };

        let fmt_lower = format.unwrap_or("all").trim().to_lowercase();
        if fmt_lower == "all" {
            // Merge top GGUF + MLX results so "All Formats" stays strictly focused on runnable local models
            let per_kind = (limit / 2).max(6);
            let (gguf_res, mlx_res) = tokio::join!(
                self.query_hf_api(query, Some("gguf"), sort_param, per_kind),
                self.query_hf_api(query, Some("mlx"), sort_param, per_kind),
            );
            let mut merged = Vec::new();
            let mut seen = std::collections::HashSet::new();
            if let Ok(list) = gguf_res {
                for item in list {
                    if seen.insert(item.id.clone()) {
                        merged.push(item);
                    }
                }
            }
            if let Ok(list) = mlx_res {
                for item in list {
                    if seen.insert(item.id.clone()) {
                        merged.push(item);
                    }
                }
            }
            if merged.is_empty() {
                // Fallback direct search without format filter if neither returned
                return self.query_hf_api(query, None, sort_param, limit).await;
            }
            sort_summaries(&mut merged, sort_param);
            merged.truncate(limit);
            return Ok(merged);
        }

        let filter_tag = match fmt_lower.as_str() {
            "gguf" => Some("gguf"),
            "mlx" => Some("mlx"),
            _ => None,
        };
        self.query_hf_api(query, filter_tag, sort_param, limit)
            .await
    }

    async fn query_hf_api(
        &self,
        query: &str,
        filter_tag: Option<&str>,
        sort_param: &str,
        limit: usize,
    ) -> Result<Vec<HfModelSummary>, ModelError> {
        let mut url = format!(
            "https://huggingface.co/api/models?sort={}&direction=-1&limit={}",
            sort_param, limit
        );
        if !query.trim().is_empty() {
            url.push_str(&format!("&search={}", urlencoding(query.trim())));
        }
        if let Some(tag) = filter_tag {
            url.push_str(&format!("&filter={}", urlencoding(tag)));
        }

        debug!(url = %url, "Searching Hugging Face models");
        let resp = self
            .client
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

        let mut models = resp
            .json::<Vec<HfModelSummary>>()
            .await
            .map_err(|e| ModelError::Http {
                url,
                source: e,
            })?;

        for m in &mut models {
            enrich_summary(m, filter_tag);
        }

        // Resolve author / organization avatars concurrently (with in-memory cache)
        let unique_authors: Vec<String> = {
            let mut seen = HashSet::new();
            let mut list = Vec::new();
            for m in &models {
                if let Some(ref author) = m.author {
                    if !author.trim().is_empty() && seen.insert(author.clone()) {
                        list.push(author.clone());
                    }
                }
            }
            list
        };

        if !unique_authors.is_empty() {
            let resolved = join_all(unique_authors.iter().map(|author| async move {
                let url = self.fetch_author_avatar(author).await;
                (author.clone(), url)
            }))
            .await;

            let map: HashMap<String, Option<String>> = resolved.into_iter().collect();
            for m in &mut models {
                if m.avatar_url.is_none() {
                    if let Some(ref author) = m.author {
                        if let Some(Some(url)) = map.get(author) {
                            m.avatar_url = Some(url.clone());
                        }
                    }
                }
            }
        }

        Ok(models)
    }

    /// Resolves a Hugging Face organization or user avatar URL via official HF avatar endpoints,
    /// caching hits & misses in memory so repeat searches incur zero latency.
    pub async fn fetch_author_avatar(&self, author: &str) -> Option<String> {
        let key = author.trim();
        if key.is_empty() || key.contains('/') || key.contains("..") {
            return None;
        }

        {
            let guard = avatar_cache().read().await;
            if let Some(cached) = guard.get(key) {
                return cached.clone();
            }
        }

        let encoded = urlencoding(key);
        let endpoints = [
            format!("https://huggingface.co/api/organizations/{encoded}/avatar"),
            format!("https://huggingface.co/api/users/{encoded}/avatar"),
        ];

        let mut found: Option<String> = None;
        for endpoint in &endpoints {
            let fut = self.client.get(endpoint).headers(self.headers()).send();
            if let Ok(Ok(resp)) =
                tokio::time::timeout(Duration::from_millis(HF_AVATAR_TIMEOUT_MS), fut).await
            {
                if resp.status().is_success() {
                    if let Ok(parsed) = resp.json::<HfAvatarResponse>().await {
                        if let Some(raw_url) = parsed.avatar_url {
                            if let Some(norm) = normalize_hf_avatar_url(&raw_url) {
                                found = Some(norm);
                                break;
                            }
                        }
                    }
                }
            }
        }

        let mut guard = avatar_cache().write().await;
        guard.insert(key.to_string(), found.clone());
        found
    }

    /// Inspects downloadable variants in a Hugging Face repository recursively (`?recursive=true`)
    /// so `.gguf` and `.safetensors` files inside subfolders are discovered, grading memory fit
    /// and excluding auxiliary files (`mmproj`, `mtp-`, `draft`, `imatrix`) from `is_recommended`.
    pub async fn inspect_gguf_files(
        &self,
        repo_id: &str,
        profile: &HardwareProfile,
    ) -> Result<Vec<HfGgufFile>, ModelError> {
        let url = format!(
            "https://huggingface.co/api/models/{}/tree/main?recursive=true",
            repo_id
        );

        debug!(repo_id = %repo_id, url = %url, "Inspecting repository files recursively");
        let resp = self
            .client
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

        let items = resp
            .json::<Vec<HfTreeItem>>()
            .await
            .map_err(|e| ModelError::Http {
                url: url.clone(),
                source: e,
            })?;

        Ok(classify_repo_tree_items(repo_id, &items, profile))
    }

    /// Downloads a repository's raw `README.md` from Hugging Face and strips YAML frontmatter.
    pub async fn fetch_readme(&self, repo_id: &str) -> Result<String, ModelError> {
        let trimmed = repo_id.trim();
        if trimmed.is_empty() || trimmed.contains("..") {
            return Err(ModelError::InvalidInput {
                reason: format!("Invalid Hugging Face repository ID: '{repo_id}'"),
            });
        }
        let url = format!("https://huggingface.co/{trimmed}/raw/main/README.md");
        debug!(url = %url, "Fetching Hugging Face README.md");

        let resp = self
            .client
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .map_err(|e| ModelError::Http {
                url: url.clone(),
                source: e,
            })?;

        if !resp.status().is_success() {
            return Err(ModelError::HuggingFace {
                url,
                message: format!("README unavailable (HTTP {})", resp.status()),
            });
        }

        let raw = resp.text().await.map_err(|e| ModelError::Http {
            url,
            source: e,
        })?;
        let cleaned = strip_yaml_frontmatter(&raw);
        if cleaned.len() > MAX_README_BYTES {
            let truncated: String = cleaned.chars().take(MAX_README_BYTES).collect();
            Ok(format!("{truncated}\n\n_(README truncated for display)_"))
        } else {
            Ok(cleaned)
        }
    }
}

fn enrich_summary(m: &mut HfModelSummary, fallback_filter: Option<&str>) {
    let lower_id = m.id.to_lowercase();
    let lower_tags: Vec<String> = m.tags.iter().map(|t| t.to_lowercase()).collect();

    let mut formats = Vec::new();
    if lower_id.contains("gguf")
        || lower_tags.iter().any(|t| t == "gguf")
        || fallback_filter == Some("gguf")
    {
        formats.push("GGUF".to_string());
    }
    if lower_id.contains("mlx")
        || lower_tags.iter().any(|t| t == "mlx")
        || fallback_filter == Some("mlx")
    {
        formats.push("MLX".to_string());
    }
    if formats.is_empty() && lower_tags.iter().any(|t| t == "safetensors") {
        formats.push("PT".to_string());
    }
    m.formats = formats;

    let has_vision_tag = lower_tags
        .iter()
        .any(|t| t.contains("vision") || t == "image-text-to-text" || t == "multimodal")
        || m.pipeline_tag
            .as_deref()
            .is_some_and(|p| p.contains("image-text"));
    m.capabilities = infer_capabilities(&m.id, None, has_vision_tag);
    if m.author.is_none() {
        m.author = m.id.split('/').next().map(String::from);
    }
}

fn sort_summaries(list: &mut [HfModelSummary], sort_param: &str) {
    match sort_param {
        "likes" => list.sort_by_key(|a| std::cmp::Reverse(a.likes.unwrap_or(0))),
        "lastModified" => list.sort_by(|a, b| {
            b.last_modified
                .as_deref()
                .unwrap_or("")
                .cmp(a.last_modified.as_deref().unwrap_or(""))
        }),
        _ => list.sort_by_key(|a| std::cmp::Reverse(a.downloads.unwrap_or(0))),
    }
}

/// Strips leading YAML frontmatter (`---\n...\n---`) from a Hugging Face `README.md`.
pub fn strip_yaml_frontmatter(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n");
    let trimmed_start = normalized.trim_start_matches('\u{feff}');
    if let Some(after_open) = trimmed_start.strip_prefix("---\n") {
        if let Some(end_idx) = after_open.find("\n---") {
            let remainder = &after_open[end_idx + 4..];
            return remainder.trim_start_matches(['\n', '\r']).to_string();
        }
    }
    trimmed_start.to_string()
}

/// Detects GGUF, Unsloth dynamic (`UD-*`), floating-point (`MXFP4`, `BF16`, `FP16`),
/// and MLX bit-width (`4bit`, `8bit`, `6bit`, `3bit`, `2bit`) quantization tags.
pub fn parse_quantization(filename: &str) -> String {
    let upper = filename.to_uppercase();

    // Checked strictly in descending order of specificity so `UD-Q4_K_XL` beats `Q4_K`, etc.
    let quant_patterns = [
        // Unsloth Dynamic 2.0 / 1.0 quantizations
        "UD-Q8_K_XL", "UD-Q8_K_L", "UD-Q6_K_XL", "UD-Q5_K_XL", "UD-Q4_K_XL",
        "UD-Q3_K_XL", "UD-Q2_K_XL",
        "UD-IQ3_XXS", "UD-IQ3_XS", "UD-IQ2_XXS", "UD-IQ2_XS", "UD-IQ2_M",
        "UD-IQ1_S", "UD-IQ1_M",
        // Extended XL / L K-quants
        "Q8_K_XL", "Q8_K_L", "Q6_K_XL", "Q5_K_XL", "Q4_K_XL", "Q3_K_XL", "Q2_K_XL",
        // Standard GGUF K-quants & legacy quants
        "Q4_K_M", "Q4_K_S", "Q4_K_L", "Q5_K_M", "Q5_K_S", "Q5_K_L",
        "Q3_K_L", "Q3_K_M", "Q3_K_S", "Q2_K_L", "Q2_K",
        "Q6_K", "Q8_0", "Q8_1", "Q5_0", "Q5_1", "Q4_0", "Q4_1",
        // Importance-matrix (IQ) quants
        "IQ4_XS", "IQ4_NL", "IQ3_XXS", "IQ3_XS", "IQ3_M", "IQ3_S",
        "IQ2_XXS", "IQ2_XS", "IQ2_M", "IQ2_S", "IQ1_S", "IQ1_M",
        // Microscaling / floating-point formats
        "MXFP4", "FP8", "BF16", "FP16", "F16", "F32",
    ];

    for pattern in quant_patterns {
        if upper.contains(pattern) {
            return pattern.to_string();
        }
    }

    // MLX bit-width tags (e.g. `-4bit`, `_8bit`, `-4-bit`, `6bit`)
    let bit_patterns = [
        ("8BIT", "8bit"),
        ("8-BIT", "8bit"),
        ("6BIT", "6bit"),
        ("6-BIT", "6bit"),
        ("4BIT", "4bit"),
        ("4-BIT", "4bit"),
        ("3BIT", "3bit"),
        ("3-BIT", "3bit"),
        ("2BIT", "2bit"),
        ("2-BIT", "2bit"),
    ];
    for (pat, canonical) in bit_patterns {
        if upper.contains(pat) {
            return canonical.to_string();
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

fn fit_rank(fit: MemoryFit) -> i32 {
    match fit {
        MemoryFit::Fits => 3,
        MemoryFit::Tight => 2,
        MemoryFit::TooLarge => 1,
    }
}

fn preference_score(quant: &str) -> i32 {
    match quant {
        "UD-Q4_K_XL" => 12,
        "Q4_K_M" => 11,
        "4bit" => 10,
        "Q5_K_M" => 9,
        "UD-Q6_K_XL" => 8,
        "Q4_K_S" => 8,
        "8bit" | "Q8_0" | "UD-Q8_K_XL" => 7,
        "Q6_K" => 6,
        "Q5_K_S" => 5,
        "MXFP4" | "IQ4_XS" | "Q4_0" | "Q4_1" => 4,
        "Q3_K_M" | "UD-IQ3_XXS" => 3,
        _ => 1,
    }
}

fn urlencoding(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.' || byte == b'~'
        {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{:02X}", byte));
        }
    }
    encoded
}

/// Normalizes a Hugging Face avatar URL (allowing only safe `https://`, `http://`, or leading `/`).
pub fn normalize_hf_avatar_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        Some(trimmed.to_string())
    } else if trimmed.starts_with("//") {
        Some(format!("https:{trimmed}"))
    } else if trimmed.starts_with('/') {
        Some(format!("https://huggingface.co{trimmed}"))
    } else {
        None
    }
}

/// Resolves relative image URLs inside a Hugging Face `README.md` against
/// `https://huggingface.co/<repo_id>/resolve/main/<rel_path>`.
pub fn resolve_hf_image_url(repo_id: &str, raw_src: &str) -> String {
    let trimmed = raw_src.trim();
    if trimmed.starts_with("https://")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("data:image/")
    {
        return trimmed.to_string();
    }
    if trimmed.starts_with("//") {
        return format!("https:{trimmed}");
    }
    let clean_rel = trimmed
        .trim_start_matches("./")
        .trim_start_matches('/');
    let clean_repo = repo_id.trim().trim_matches('/');
    format!("https://huggingface.co/{clean_repo}/resolve/main/{clean_rel}")
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
        assert_eq!(parse_quantization("model-fp16.gguf"), "FP16");
        assert_eq!(parse_quantization("custom-model.gguf"), "UNKNOWN");

        // Unsloth Dynamic & modern formats (Phase 11)
        assert_eq!(parse_quantization("Qwen3-30B-UD-Q4_K_XL.gguf"), "UD-Q4_K_XL");
        assert_eq!(parse_quantization("Qwen3-30B-UD-Q6_K_XL.gguf"), "UD-Q6_K_XL");
        assert_eq!(parse_quantization("Qwen3-30B-UD-Q8_K_L.gguf"), "UD-Q8_K_L");
        assert_eq!(parse_quantization("Qwen3-30B-UD-Q8_K_XL.gguf"), "UD-Q8_K_XL");
        assert_eq!(parse_quantization("DeepSeek-R1-UD-IQ1_S.gguf"), "UD-IQ1_S");
        assert_eq!(parse_quantization("DeepSeek-R1-UD-IQ2_XXS.gguf"), "UD-IQ2_XXS");
        assert_eq!(parse_quantization("Llama-4-UD-IQ3_XXS.gguf"), "UD-IQ3_XXS");
        assert_eq!(parse_quantization("model-Q4_1.gguf"), "Q4_1");
        assert_eq!(parse_quantization("gpt-oss-20b-MXFP4.gguf"), "MXFP4");
        assert_eq!(parse_quantization("mlx-community/Qwen3-Coder-Next-4bit"), "4bit");
        assert_eq!(parse_quantization("mlx-community/Qwen3-Coder-Next-8bit"), "8bit");
    }

    #[test]
    fn test_strip_yaml_frontmatter() {
        let raw = "---\nlicense: apache-2.0\ntags:\n  - mlx\n  - gguf\n---\n\n# Qwen3 Coder\n\nAwesome model.\n";
        let clean = strip_yaml_frontmatter(raw);
        assert!(clean.starts_with("# Qwen3 Coder"));
        assert!(!clean.contains("license: apache-2.0"));

        let no_fm = "# Direct Title\nBody text";
        assert_eq!(strip_yaml_frontmatter(no_fm), no_fm);
    }

    #[test]
    fn test_evaluate_memory_fit() {
        let profile = HardwareProfile {
            total_memory_gb: 32.0,
            used_memory_gb: 16.0,
            available_memory_gb: 16.0,
            usage_percent: 50.0,
            cpu_cores: 10,
            os_name: "macOS".to_string(),
            arch: "aarch64".to_string(),
            is_apple_silicon: true,
        };

        let (fit1, req1) = evaluate_memory_fit(4 * 1024 * 1024 * 1024, &profile);
        assert_eq!(fit1, MemoryFit::Fits);
        assert_eq!(req1, 4.8);

        let (fit2, req2) = evaluate_memory_fit(18 * 1024 * 1024 * 1024, &profile);
        assert_eq!(fit2, MemoryFit::Tight);
        assert_eq!(req2, 21.6);

        let (fit3, req3) = evaluate_memory_fit(40 * 1024 * 1024 * 1024, &profile);
        assert_eq!(fit3, MemoryFit::TooLarge);
        assert_eq!(req3, 48.0);
    }

    #[test]
    fn test_resolve_hf_image_url_and_avatar_norm() {
        assert_eq!(
            resolve_hf_image_url("unsloth/Qwen3-30B-GGUF", "./assets/benchmark.png"),
            "https://huggingface.co/unsloth/Qwen3-30B-GGUF/resolve/main/assets/benchmark.png"
        );
        assert_eq!(
            resolve_hf_image_url("mlx-community/Qwen2.5-7B-4bit", "images/chart.webp"),
            "https://huggingface.co/mlx-community/Qwen2.5-7B-4bit/resolve/main/images/chart.webp"
        );
        assert_eq!(
            resolve_hf_image_url("unsloth/Qwen3-30B-GGUF", "https://example.com/banner.png"),
            "https://example.com/banner.png"
        );
        assert_eq!(
            resolve_hf_image_url("unsloth/Qwen3-30B-GGUF", "//cdn.example.com/logo.svg"),
            "https://cdn.example.com/logo.svg"
        );

        assert_eq!(
            normalize_hf_avatar_url("/avatars/abc123.svg").as_deref(),
            Some("https://huggingface.co/avatars/abc123.svg")
        );
        assert_eq!(
            normalize_hf_avatar_url("https://cdn-avatars.huggingface.co/v1/x.png").as_deref(),
            Some("https://cdn-avatars.huggingface.co/v1/x.png")
        );
        assert_eq!(normalize_hf_avatar_url("javascript:alert(1)"), None);
    }
}
