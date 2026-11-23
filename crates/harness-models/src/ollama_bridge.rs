use std::path::Path;
use std::time::Instant;
use futures::StreamExt;
use harness_core::{DexEvent, EventBus};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::error::ModelError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaModelDetails {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub parameter_size: Option<String>,
    #[serde(default)]
    pub quantization_level: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaModelTag {
    pub name: String,
    #[serde(default)]
    pub modified_at: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub details: Option<OllamaModelDetails>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    pub models: Vec<OllamaModelTag>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OllamaPullProgress {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub completed: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct OllamaBridge {
    client: reqwest::Client,
    base_url: String,
}

impl Default for OllamaBridge {
    fn default() -> Self {
        Self::new("http://localhost:11434")
    }
}

impl OllamaBridge {
    pub fn new(base_url: impl Into<String>) -> Self {
        let mut url = base_url.into();
        if url.ends_with('/') {
            url.pop();
        }
        Self {
            client: reqwest::Client::builder()
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            base_url: url,
        }
    }

    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("Dex-Harness/0.5.0"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers
    }

    /// Lists all models currently installed in the local Ollama instance.
    pub async fn list_models(&self) -> Result<Vec<OllamaModelTag>, ModelError> {
        let endpoint = format!("{}/api/tags", self.base_url);
        debug!(endpoint = %endpoint, "Fetching Ollama tags");

        let resp = self.client
            .get(&endpoint)
            .headers(Self::headers())
            .send()
            .await
            .map_err(|e| ModelError::Http {
                url: endpoint.clone(),
                source: e,
            })?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let msg = resp.text().await.unwrap_or_default();
            return Err(ModelError::Ollama {
                endpoint,
                status,
                message: msg,
            });
        }

        let tags_resp = resp.json::<OllamaTagsResponse>().await.map_err(|e| ModelError::Http {
            url: endpoint,
            source: e,
        })?;

        Ok(tags_resp.models)
    }

    /// Pulls a model from the official Ollama library with streaming progress.
    pub async fn pull_model(
        &self,
        model_name: &str,
        event_bus: Option<&EventBus>,
        cancel: CancellationToken,
    ) -> Result<(), ModelError> {
        let endpoint = format!("{}/api/pull", self.base_url);
        info!(model = %model_name, endpoint = %endpoint, "Initiating Ollama pull");

        let body = serde_json::json!({
            "name": model_name,
            "stream": true,
        });

        let resp = self.client
            .post(&endpoint)
            .headers(Self::headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Http {
                url: endpoint.clone(),
                source: e,
            })?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let msg = resp.text().await.unwrap_or_default();
            return Err(ModelError::Ollama {
                endpoint,
                status,
                message: msg,
            });
        }

        let mut stream = resp.bytes_stream();
        let start_time = Instant::now();
        let mut last_completed = 0u64;
        let mut last_event_time = Instant::now();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    warn!(model = %model_name, "Ollama pull cancelled by user");
                    if let Some(bus) = event_bus {
                        bus.publish(DexEvent::ModelDownloadFailed {
                            model_id: format!("ollama:{}", model_name),
                            file_name: model_name.to_string(),
                            error: "Cancelled by user".to_string(),
                        });
                    }
                    return Err(ModelError::DownloadCancelled {
                        model_id: format!("ollama:{}", model_name),
                        reason: "Pull cancelled".to_string(),
                    });
                }
                chunk_opt = stream.next() => {
                    match chunk_opt {
                        Some(Ok(bytes)) => {
                            let text = String::from_utf8_lossy(&bytes);
                            for line in text.lines() {
                                let trimmed = line.trim();
                                if trimmed.is_empty() {
                                    continue;
                                }

                                if let Ok(progress) = serde_json::from_str::<OllamaPullProgress>(trimmed) {
                                    if let Some(ref err) = progress.error {
                                        return Err(ModelError::Ollama {
                                            endpoint: endpoint.clone(),
                                            status: 400,
                                            message: err.clone(),
                                        });
                                    }

                                    if let (Some(completed), Some(total)) = (progress.completed, progress.total) {
                                        let now = Instant::now();
                                        let elapsed = now.duration_since(last_event_time).as_secs_f64();

                                        if elapsed >= 0.25 || completed == total {
                                            let speed_mbps = if elapsed > 0.0 && completed >= last_completed {
                                                ((completed - last_completed) as f64 / elapsed) / (1024.0 * 1024.0)
                                            } else {
                                                0.0
                                            };

                                            if let Some(bus) = event_bus {
                                                bus.publish(DexEvent::ModelDownloadProgress {
                                                    model_id: format!("ollama:{}", model_name),
                                                    file_name: model_name.to_string(),
                                                    downloaded_bytes: completed,
                                                    total_bytes: total,
                                                    speed_mbps: (speed_mbps * 10.0).round() / 10.0,
                                                });
                                            }

                                            last_completed = completed;
                                            last_event_time = now;
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => {
                            return Err(ModelError::Http {
                                url: endpoint,
                                source: e,
                            });
                        }
                        None => break,
                    }
                }
            }
        }

        info!(
            model = %model_name,
            duration = %format!("{:.1}s", start_time.elapsed().as_secs_f64()),
            "Ollama model pull completed"
        );

        if let Some(bus) = event_bus {
            bus.publish(DexEvent::ModelDownloadCompleted {
                model_id: format!("ollama:{}", model_name),
                file_name: model_name.to_string(),
                total_bytes: last_completed,
            });
        }

        Ok(())
    }

    /// Deletes a model from the local Ollama instance.
    pub async fn delete_model(&self, model_name: &str) -> Result<(), ModelError> {
        let endpoint = format!("{}/api/delete", self.base_url);
        info!(model = %model_name, endpoint = %endpoint, "Deleting Ollama model");

        let body = serde_json::json!({ "name": model_name });

        let resp = self.client
            .delete(&endpoint)
            .headers(Self::headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Http {
                url: endpoint.clone(),
                source: e,
            })?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let msg = resp.text().await.unwrap_or_default();
            return Err(ModelError::Ollama {
                endpoint,
                status,
                message: msg,
            });
        }

        Ok(())
    }

    /// Creates or registers a new model in Ollama pointing directly to a local GGUF file.
    pub async fn create_from_gguf(&self, model_name: &str, gguf_path: &Path) -> Result<(), ModelError> {
        let endpoint = format!("{}/api/create", self.base_url);
        let abs_path = if gguf_path.is_absolute() {
            gguf_path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(gguf_path))
                .unwrap_or_else(|_| gguf_path.to_path_buf())
        };

        info!(model = %model_name, path = ?abs_path, "Creating Ollama model from local GGUF");

        let modelfile = format!("FROM \"{}\"\n", abs_path.to_string_lossy());
        let body = serde_json::json!({
            "name": model_name,
            "modelfile": modelfile,
            "stream": false,
        });

        let resp = self.client
            .post(&endpoint)
            .headers(Self::headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Http {
                url: endpoint.clone(),
                source: e,
            })?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let msg = resp.text().await.unwrap_or_default();
            return Err(ModelError::Ollama {
                endpoint,
                status,
                message: msg,
            });
        }

        info!(model = %model_name, "Model created in Ollama successfully");
        Ok(())
    }
}
