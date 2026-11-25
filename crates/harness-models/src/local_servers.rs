use std::time::Duration;
use serde::{Deserialize, Serialize};

use crate::ollama_bridge::OllamaBridge;

pub const LM_STUDIO_DEFAULT_URL: &str = "http://127.0.0.1:1234/v1";
pub const LM_STUDIO_CANDIDATE_PORTS: [u16; 3] = [1234, 1235, 1236];
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

    // Absent servers refuse the connection instantly; a running LM Studio can take >1s to
    // enumerate a large model library, so allow a generous body timeout.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(400))
        .timeout(Duration::from_secs(4))
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

/// LM Studio falls back to the next free port (`:1235`, `:1236`) when `:1234` is taken,
/// so probe each candidate until one answers (unless `DEX_LMSTUDIO_URL` pins it).
async fn probe_lm_studio(pinned: Option<String>) -> LocalServerInfo {
    let candidates: Vec<String> = match pinned {
        Some(url) => vec![url],
        None => LM_STUDIO_CANDIDATE_PORTS
            .iter()
            .map(|p| format!("http://127.0.0.1:{p}/v1"))
            .collect(),
    };
    let mut first: Option<LocalServerInfo> = None;
    for url in &candidates {
        let info = probe_openai_server("lmstudio", "LM Studio", url).await;
        if info.reachable {
            return info;
        }
        first.get_or_insert(info);
    }
    first.unwrap_or_else(|| LocalServerInfo {
        id: "lmstudio".to_string(),
        name: "LM Studio".to_string(),
        provider: "openai".to_string(),
        base_url: LM_STUDIO_DEFAULT_URL.to_string(),
        reachable: false,
        loaded_models: Vec::new(),
    })
}

/// Returns `true` when a served model id looks like a chat/instruct LLM rather than an
/// embedding, speech, or TTS model (LM Studio lists every downloaded model in `/v1/models`).
pub fn is_chat_model_id(id: &str) -> bool {
    let lower = id.to_lowercase();
    const NON_CHAT: [&str; 12] = [
        "embed", "embedding", "rerank", "whisper", "voxtral", "parakeet", "tts", "phonon",
        "kokoro", "stt", "clip", "bge-",
    ];
    !NON_CHAT.iter().any(|needle| lower.contains(needle))
}

/// Picks the best local chat endpoint: the first reachable server (MLX → llama.cpp → Ollama →
/// LM Studio order from [`detect_local_servers`]) that serves at least one chat model.
/// When `preferred_model` is served by a server, that server wins.
pub fn pick_chat_endpoint(
    servers: &[LocalServerInfo],
    preferred_model: Option<&str>,
) -> Option<(LocalServerInfo, String)> {
    if let Some(pref) = preferred_model.filter(|m| !m.trim().is_empty()) {
        for srv in servers.iter().filter(|s| s.reachable) {
            if srv.loaded_models.iter().any(|m| m == pref) {
                return Some((srv.clone(), pref.to_string()));
            }
        }
    }
    servers.iter().filter(|s| s.reachable).find_map(|srv| {
        srv.loaded_models
            .iter()
            .find(|m| is_chat_model_id(m))
            .map(|m| (srv.clone(), m.clone()))
    })
}

/// Probes LM Studio / Bionic (`:1234/v1`), Ollama (`:11434`), and local MLX-LM server (`:8088/v1`)
/// concurrently without blocking.
pub async fn detect_local_servers() -> Vec<LocalServerInfo> {
    let lm_env = std::env::var("DEX_LMSTUDIO_URL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let mlx_url = std::env::var("DEX_MLX_LM_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("http://127.0.0.1:{MLX_LM_DEFAULT_PORT}/v1"));
    let llama_url = std::env::var("DEX_LLAMA_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("http://127.0.0.1:{LLAMA_SERVER_DEFAULT_PORT}/v1"));

    let (lm_studio, ollama, mlx_lm, llama_srv) = tokio::join!(
        probe_lm_studio(lm_env),
        probe_ollama_server(),
        probe_openai_server("mlx-lm", "Dex MLX (:8088)", &mlx_url),
        probe_openai_server("llamacpp", "Dex llama.cpp (:8089)", &llama_url),
    );

    vec![mlx_lm, llama_srv, ollama, lm_studio]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(id: &str, reachable: bool, models: &[&str]) -> LocalServerInfo {
        LocalServerInfo {
            id: id.to_string(),
            name: id.to_string(),
            provider: "openai".to_string(),
            base_url: format!("http://127.0.0.1/{id}"),
            reachable,
            loaded_models: models.iter().map(|m| m.to_string()).collect(),
        }
    }

    #[test]
    fn filters_non_chat_models() {
        assert!(is_chat_model_id("qwen/qwen3.8-27b"));
        assert!(!is_chat_model_id("text-embedding-nomic-embed-text-v1.5"));
        assert!(!is_chat_model_id("voxtral-realtime-et"));
        assert!(!is_chat_model_id("phonon-40m-multilingual"));
    }

    #[test]
    fn picks_preferred_then_first_chat_model() {
        let servers = vec![
            srv("mlx", false, &[]),
            srv("lmstudio", true, &["voxtral-realtime-et", "gemma", "qwen"]),
        ];
        let (s, m) = pick_chat_endpoint(&servers, None).expect("endpoint");
        assert_eq!((s.id.as_str(), m.as_str()), ("lmstudio", "gemma"));
        let (_, m) = pick_chat_endpoint(&servers, Some("qwen")).expect("endpoint");
        assert_eq!(m, "qwen");
        assert!(pick_chat_endpoint(&[srv("x", false, &["a"])], None).is_none());
    }
}
