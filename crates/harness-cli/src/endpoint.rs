//! Zero-config model endpoint resolution for the `dex` CLI.
//!
//! `dex` with no flags should "just work" against whatever local server is running
//! (LM Studio on :1234/:1235, Ollama, Dex MLX, llama.cpp), like `claude` just works.
//! Explicit flags / env vars / `~/.dex/cli.json` always win over auto-detection.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use harness_core::{find_executable_in_enriched_path, home_dex_dir, LlmProvider};
use harness_llm::{OllamaProvider, OpenAiProvider};
use harness_models::{detect_local_servers, is_chat_model_id, pick_chat_endpoint, LocalServerInfo};

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

/// A fully resolved chat endpoint.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// `ollama` or `openai` (any OpenAI-compatible server).
    pub kind: String,
    pub model: String,
    pub base_url: Option<String>,
    /// Human label, e.g. "LM Studio · 127.0.0.1:1235".
    pub label: String,
    /// Whether this points at a local server (no API key needed).
    pub local: bool,
}

impl Endpoint {
    pub fn build_provider(&self) -> Arc<dyn LlmProvider> {
        match self.kind.as_str() {
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
    }
}

/// Tries to start the LM Studio headless server if the `lms` CLI is installed.
async fn try_start_lm_studio(status: &dyn Fn(&str)) -> bool {
    let lms = find_executable_in_enriched_path("lms")
        .or_else(|| dirs_lms().filter(|p| p.exists()));
    let Some(lms) = lms else { return false };
    status("Starting LM Studio server (lms server start)…");
    let mut cmd = tokio::process::Command::new(lms);
    cmd.args(["server", "start"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    matches!(
        tokio::time::timeout(Duration::from_secs(30), cmd.status()).await,
        Ok(Ok(s)) if s.success()
    )
}

fn dirs_lms() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".lmstudio/bin/lms"))
}

fn is_local_url(url: &str) -> bool {
    let host = host_of(url);
    host.starts_with("127.0.0.1") || host.starts_with("localhost") || host.starts_with("[::1]")
}

/// Resolves the endpoint to chat with. `status` receives short progress lines (write them to
/// stderr so `dex mcp-serve` keeps stdout clean).
pub async fn resolve(req: &EndpointRequest, status: &dyn Fn(&str)) -> Result<Endpoint> {
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
        });
    }

    let wanted: &[&str] = match provider.as_str() {
        "auto" => &[],
        "ollama" => &["ollama"],
        "lmstudio" | "lm_studio" | "lm-studio" => &["lmstudio"],
        "mlx" | "mlx-lm" | "dex-local" => &["mlx-lm"],
        "llamacpp" | "llama.cpp" | "llama-cpp" => &["llamacpp"],
        other => bail!(
            "Unknown provider '{other}'. Use one of: auto, lmstudio, ollama, mlx, llamacpp, openai."
        ),
    };

    let mut servers = detect_local_servers().await;
    let filter = |all: Vec<LocalServerInfo>| -> Vec<LocalServerInfo> {
        if wanted.is_empty() {
            all
        } else {
            all.into_iter().filter(|s| wanted.contains(&s.id.as_str())).collect()
        }
    };

    if !servers.iter().any(|s| s.reachable)
        && (wanted.is_empty() || wanted.contains(&"lmstudio"))
        && try_start_lm_studio(status).await
    {
        for _ in 0..10 {
            servers = detect_local_servers().await;
            if servers.iter().any(|s| s.reachable) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    let candidates = filter(servers);
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

    let reachable: Vec<&LocalServerInfo> = candidates.iter().filter(|s| s.reachable).collect();
    if let Some(srv) = reachable.first() {
        bail!(
            "{} is running but serves no chat model. Load one (e.g. `lms load <model>` or `ollama pull qwen3:8b`) and retry.",
            srv.name
        );
    }
    bail!(
        "No local model server found.\n  • LM Studio: open it, or run `lms server start`\n  • Ollama:    `ollama serve`\n  • Or point Dex at any OpenAI-compatible server: `dex --api-base http://127.0.0.1:8080/v1 --model <id>`\nRun `dex doctor` for a full diagnosis."
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
        assert!(resolve(&req, &|_| {}).await.is_err());

        let req = EndpointRequest {
            api_base: Some("http://127.0.0.1:9/v1".into()),
            model: Some("m".into()),
            ..Default::default()
        };
        let ep = resolve(&req, &|_| {}).await.expect("endpoint");
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
        let err = resolve(&req, &|_| {}).await.expect_err("should fail");
        assert!(err.to_string().contains("Unknown provider"));
    }
}
