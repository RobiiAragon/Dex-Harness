use std::time::Duration;
use serde::{Deserialize, Serialize};

use crate::ollama_bridge::OllamaBridge;

pub const LM_STUDIO_DEFAULT_URL: &str = "http://127.0.0.1:1234/v1";
pub const MLX_LM_DEFAULT_PORT: u16 = 8088;
pub const LLAMA_SERVER_DEFAULT_PORT: u16 = 8089;

/// Live status of a detected local inference server (LM Studio, Ollama, MLX-LM).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalServerInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub base_url: String,
    pub reachable: bool,
    pub loaded_models: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelsResponse {
    #[serde(default)]
    data: Vec<OpenAiModelEntry>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelEntry {
    id: String,
}

async fn probe_openai_server(id: &str, name: &str, base_url: &str) -> LocalServerInfo {
    let clean_base = base_url.trim_end_matches('/').to_string();
    let models_url = format!("{clean_base}/models");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    match client.get(&models_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let loaded = resp
                .json::<OpenAiModelsResponse>()
                .await
                .map(|r| r.data.into_iter().map(|m| m.id).collect())
                .unwrap_or_default();
            LocalServerInfo {
                id: id.to_string(),
                name: name.to_string(),
                provider: "openai".to_string(),
                base_url: clean_base,
                reachable: true,
                loaded_models: loaded,
            }
        }
        _ => LocalServerInfo {
            id: id.to_string(),
            name: name.to_string(),
            provider: "openai".to_string(),
            base_url: clean_base,
            reachable: false,
            loaded_models: Vec::new(),
        },
    }
}

async fn probe_ollama_server() -> LocalServerInfo {
    let bridge = OllamaBridge::default();
    match tokio::time::timeout(Duration::from_millis(600), bridge.list_models()).await {
        Ok(Ok(tags)) => LocalServerInfo {
            id: "ollama".to_string(),
            name: "Ollama".to_string(),
            provider: "ollama".to_string(),
            base_url: "http://127.0.0.1:11434".to_string(),
            reachable: true,
            loaded_models: tags.into_iter().map(|t| t.name).collect(),
        },
        _ => LocalServerInfo {
            id: "ollama".to_string(),
            name: "Ollama".to_string(),
            provider: "ollama".to_string(),
            base_url: "http://127.0.0.1:11434".to_string(),
            reachable: false,
            loaded_models: Vec::new(),
        },
    }
}

/// Probes LM Studio / Bionic (`:1234/v1`), Ollama (`:11434`), and local MLX-LM server (`:8088/v1`)
/// concurrently without blocking.
pub async fn detect_local_servers() -> Vec<LocalServerInfo> {
    let lm_url = std::env::var("DEX_LMSTUDIO_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| LM_STUDIO_DEFAULT_URL.to_string());
    let mlx_url = std::env::var("DEX_MLX_LM_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("http://127.0.0.1:{MLX_LM_DEFAULT_PORT}/v1"));
    let llama_url = std::env::var("DEX_LLAMA_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("http://127.0.0.1:{LLAMA_SERVER_DEFAULT_PORT}/v1"));

    let (lm_studio, ollama, mlx_lm, llama_srv) = tokio::join!(
        probe_openai_server("lmstudio", "LM Studio / Bionic", &lm_url),
        probe_ollama_server(),
        probe_openai_server("mlx-lm", "Dex MLX (:8088)", &mlx_url),
        probe_openai_server("llamacpp", "Dex llama.cpp (:8089)", &llama_url),
    );

    vec![mlx_lm, llama_srv, ollama, lm_studio]
}
