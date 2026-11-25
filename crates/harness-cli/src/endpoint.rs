//! Zero-config model endpoint resolution for the `dex` CLI.
//!
//! `dex` with no flags should "just work": it runs a downloaded local model with Dex's own
//! MLX / llama.cpp runtime, sized to this machine, without needing any other app.
//! Explicit flags / env vars / `~/.dex/cli.json` always win over auto-detection.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use harness_core::{home_dex_dir, LlmProvider};
use harness_extensions::ExtensionSupervisor;
use harness_llm::{OllamaProvider, OpenAiProvider};
use harness_models::{
    detect_local_servers, discover_native_chat_models, find_local_model, is_chat_model_id,
    load_native_chat_model, pick_chat_endpoint, pick_default_chat_model, total_ram_bytes,
    ActiveLoadedModel, DiscoveredModel, LocalServerInfo, NativeReloadProvider,
};

/// Persistent CLI preferences (`~/.dex/cli.json`), written by `/model` and `/mode` in the REPL.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CliConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl CliConfig {
    pub fn path() -> PathBuf {
        home_dex_dir().join("cli.json")
    }

    pub async fn load() -> Self {
        match tokio::fs::read_to_string(Self::path()).await {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub async fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        let raw = serde_json::to_string_pretty(self).context("Failed to serialize CLI config")?;
        tokio::fs::write(&path, raw)
            .await
            .with_context(|| format!("Failed to write {}", path.display()))
    }
}

/// Dex's own chat runtime backing an endpoint (the model reloads by itself when unloaded).
#[derive(Clone)]
pub struct NativeHandle {
    pub supervisor: Arc<ExtensionSupervisor>,
    pub model: ActiveLoadedModel,
}

impl std::fmt::Debug for NativeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeHandle").field("model", &self.model.name).finish()
    }
}

/// A fully resolved chat endpoint.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// `ollama` or `openai` (any OpenAI-compatible server).
    pub kind: String,
    pub model: String,
    pub base_url: Option<String>,
    /// Human label, e.g. "Dex runtime · Qwen3.5 35B".
    pub label: String,
    /// Whether this points at a local server (no API key needed).
    pub local: bool,
    /// Set when the model runs in Dex's own runtime (loaded by this process).
    pub native: Option<NativeHandle>,
}

impl Endpoint {
    /// Short model name for display (Dex's runtime addresses models by their full path).
    pub fn display_model(&self) -> &str {
        match &self.native {
            Some(native) => &native.model.name,
            None => &self.model,
        }
    }

    pub fn build_provider(&self) -> Arc<dyn LlmProvider> {
        let base: Arc<dyn LlmProvider> = match self.kind.as_str() {
            "ollama" => {
                let mut p = OllamaProvider::new(&self.model);
                if let Some(ref base) = self.base_url {
                    p = p.with_base_url(base);
                }
                Arc::new(p)
            }
            _ => {
                let mut p = if self.local {
                    OpenAiProvider::lm_studio(&self.model)
                } else {
                    OpenAiProvider::new(&self.model)
                };
                if let Some(ref base) = self.base_url {
                    p = p.with_base_url(base);
                }
                Arc::new(p)
            }
        };
        match &self.native {
            Some(native) => Arc::new(NativeReloadProvider::new(
                base,
                native.supervisor.clone(),
                native.model.sidecar_id.clone(),
            )),
            None => base,
        }
    }
}

/// Requested endpoint settings, merged from flags/env and `~/.dex/cli.json`.
#[derive(Debug, Clone, Default)]
pub struct EndpointRequest {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_base: Option<String>,
}

fn host_of(url: &str) -> String {
    url.trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

fn from_server(srv: &LocalServerInfo, model: String) -> Endpoint {
    Endpoint {
        kind: srv.provider.clone(),
        model,
        base_url: Some(srv.base_url.clone()),
        label: format!("{} · {}", srv.name, host_of(&srv.base_url)),
        local: true,
        native: None,
    }
}

fn is_local_url(url: &str) -> bool {
    let host = host_of(url);
    host.starts_with("127.0.0.1") || host.starts_with("localhost") || host.starts_with("[::1]")
}

/// Model id to send to Dex's runtime: mlx_lm serves the model by its path, llama-server by file.
fn native_request_model(active: &ActiveLoadedModel) -> String {
    if active.runtime == "mlx" {
        active.path.clone()
    } else {
        std::path::Path::new(&active.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&active.name)
            .to_string()
    }
}

/// Picks the local model to run in Dex's runtime: the preferred one if it exists on disk,
/// otherwise the best fit for this machine's RAM.
pub async fn choose_native_model(
    preferred: Option<&str>,
    status: &dyn Fn(&str),
) -> Option<DiscoveredModel> {
    let models = discover_native_chat_models().await;
    if let Some(pref) = preferred {
        if let Some(m) = find_local_model(&models, pref) {
            return Some(m.clone());
        }
    }
    let ram = total_ram_bytes().await;
    let picked = pick_default_chat_model(&models, ram).cloned();
    if let (Some(pref), Some(m)) = (preferred, picked.as_ref()) {
        status(&format!("Model '{pref}' is not on this Mac; using {} instead.", m.name));
    }
    picked
}

/// Resolves the endpoint to chat with. `status` receives short progress lines (write them to
/// stderr so `dex mcp-serve` keeps stdout clean).
///
/// Local-first and self-contained: Dex runs your downloaded MLX/GGUF models with its own
/// runtime. Other apps' servers (LM Studio, Ollama) are only used when you pick them with
/// `--provider`, or as a last resort when they are already running — Dex never launches them.
/// With `supervisor = None` (e.g. `dex doctor`) the chosen model is reported but not loaded.
pub async fn resolve(
    req: &EndpointRequest,
    status: &dyn Fn(&str),
    supervisor: Option<&Arc<ExtensionSupervisor>>,
) -> Result<Endpoint> {
    let provider = req
        .provider
        .as_deref()
        .map(str::to_lowercase)
        .unwrap_or_else(|| "auto".to_string());
    let model = req.model.clone().filter(|m| !m.trim().is_empty());

    // Explicit base URL: trust the user, no probing beyond what's needed.
    if let Some(base) = req.api_base.clone().filter(|b| !b.trim().is_empty()) {
        let kind = if provider == "ollama" { "ollama" } else { "openai" };
        let Some(model) = model else {
            bail!("--api-base was given without --model. Pass the model id, e.g. `dex --api-base {base} --model <id>`.");
        };
        return Ok(Endpoint {
            kind: kind.to_string(),
            model,
            label: host_of(&base),
            local: is_local_url(&base),
            base_url: Some(base),
            native: None,
        });
    }

    if provider == "openai" {
        let Some(model) = model else {
            bail!("Provider 'openai' needs --model (e.g. gpt-4o-mini) and OPENAI_API_KEY.");
        };
        return Ok(Endpoint {
            kind: "openai".to_string(),
            model,
            base_url: None,
            label: "OpenAI API".to_string(),
            local: false,
            native: None,
        });
    }

    let (native_allowed, wanted): (bool, &[&str]) = match provider.as_str() {
        "auto" | "dex" | "dex-local" | "native" | "local" => (true, &[]),
        "mlx" | "mlx-lm" => (true, &["mlx-lm"]),
        "llamacpp" | "llama.cpp" | "llama-cpp" => (true, &["llamacpp"]),
        "ollama" => (false, &["ollama"]),
        "lmstudio" | "lm_studio" | "lm-studio" => (false, &["lmstudio"]),
        other => bail!(
            "Unknown provider '{other}'. Use one of: auto, mlx, llamacpp, ollama, lmstudio, openai."
        ),
    };

    let mut native_error: Option<String> = None;
    if native_allowed {
        if let Some(chosen) = choose_native_model(model.as_deref(), status).await {
            let Some(sup) = supervisor else {
                return Ok(Endpoint {
                    kind: "openai".to_string(),
                    model: chosen.path.clone().unwrap_or_default(),
                    base_url: None,
                    label: format!("Dex runtime · {} (loads on first use)", chosen.name),
                    local: true,
                    native: None,
                });
            };
            status(&format!(
                "Loading {} ({:.1} GB) with Dex's runtime…",
                chosen.name,
                chosen.size_bytes as f64 / 1e9
            ));
            match load_native_chat_model(sup, &chosen, None).await {
                Ok(active) => {
                    return Ok(Endpoint {
                        kind: "openai".to_string(),
                        model: native_request_model(&active),
                        base_url: Some(active.base_url.clone()),
                        label: "Dex runtime".to_string(),
                        local: true,
                        native: Some(NativeHandle {
                            supervisor: sup.clone(),
                            model: active,
                        }),
                    });
                }
                Err(e) => {
                    status(&format!("Could not load {}: {e}", chosen.name));
                    native_error = Some(e.to_string());
                }
            }
        }
    }

    // Servers that are already running (never launched by Dex).
    let servers = detect_local_servers().await;
    let candidates: Vec<LocalServerInfo> = if wanted.is_empty() {
        servers
    } else {
        servers.into_iter().filter(|s| wanted.contains(&s.id.as_str())).collect()
    };
    if let Some((srv, chosen)) = pick_chat_endpoint(&candidates, model.as_deref()) {
        if let Some(ref pref) = model {
            if pref != &chosen {
                status(&format!("Model '{pref}' not found on running servers; using '{chosen}'."));
            }
        }
        return Ok(from_server(&srv, chosen));
    }

    // A server is up but the preferred model isn't listed (e.g. Ollama tag typed by hand).
    if let (Some(model), Some(srv)) = (model.clone(), candidates.iter().find(|s| s.reachable)) {
        return Ok(from_server(srv, model));
    }

    if let Some(err) = native_error {
        bail!("Dex could not start your local model: {err}\nRun `dex doctor` for a full diagnosis.");
    }
    if !native_allowed {
        bail!(
            "{} is not running. Start it yourself, or drop `--provider` and Dex will run a local model with its own runtime.",
            if wanted.contains(&"ollama") { "Ollama" } else { "LM Studio" }
        );
    }
    bail!(
        "No local chat model found on this Mac.\n  • Download one: `dex models pull mlx-community/Qwen3-8B-4bit`\n  • Or add a folder with models: `dex models dirs add <path>`\n  • Or point Dex at any OpenAI-compatible server: `dex --api-base http://127.0.0.1:8080/v1 --model <id>`\nRun `dex doctor` for a full diagnosis."
    )
}

/// Chat-capable models served by the endpoint's server (for `/model` listings).
pub async fn list_chat_models(endpoint: &Endpoint) -> Vec<String> {
    let servers = detect_local_servers().await;
    servers
        .into_iter()
        .filter(|s| s.reachable && Some(&s.base_url) == endpoint.base_url.as_ref())
        .flat_map(|s| s.loaded_models)
        .filter(|m| is_chat_model_id(m))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn explicit_api_base_requires_model() {
        let req = EndpointRequest {
            api_base: Some("http://127.0.0.1:9/v1".into()),
            ..Default::default()
        };
        assert!(resolve(&req, &|_| {}, None).await.is_err());

        let req = EndpointRequest {
            api_base: Some("http://127.0.0.1:9/v1".into()),
            model: Some("m".into()),
            ..Default::default()
        };
        let ep = resolve(&req, &|_| {}, None).await.expect("endpoint");
        assert_eq!(ep.kind, "openai");
        assert!(ep.local);
        assert_eq!(ep.label, "127.0.0.1:9");
    }

    #[tokio::test]
    async fn unknown_provider_is_rejected() {
        let req = EndpointRequest {
            provider: Some("nope".into()),
            ..Default::default()
        };
        let err = resolve(&req, &|_| {}, None).await.expect_err("should fail");
        assert!(err.to_string().contains("Unknown provider"));
    }
}
