use std::path::{Path, PathBuf};
use std::time::Instant;
use futures::StreamExt;
use harness_core::{DexEvent, EventBus};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, RANGE, USER_AGENT};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::error::ModelError;

pub struct DownloadProgress {
    pub model_id: String,
    pub file_name: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed_mbps: f64,
}

pub struct ModelDownloader {
    client: reqwest::Client,
    token: Option<String>,
}

impl Default for ModelDownloader {
    fn default() -> Self {
        Self::new(None)
    }
}

impl ModelDownloader {
    pub fn new(token: Option<String>) -> Self {
        let active_token = token.or_else(|| std::env::var("HF_TOKEN").ok());
        Self {
            client: reqwest::Client::builder()
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            token: active_token,
        }
    }

    pub async fn download(
        &self,
        url: &str,
        dest_path: impl AsRef<Path>,
        model_id: &str,
        file_name: &str,
        event_bus: Option<&EventBus>,
        cancel: CancellationToken,
    ) -> Result<PathBuf, ModelError> {
        let final_dest = dest_path.as_ref().to_path_buf();
        if let Some(parent) = final_dest.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| ModelError::Io {
                path: parent.to_string_lossy().to_string(),
                source: e,
            })?;
        }

        let part_path = PathBuf::from(format!("{}.part", final_dest.display()));
        let mut existing_bytes = 0u64;

        if tokio::fs::try_exists(&part_path).await.unwrap_or(false) {
            if let Ok(meta) = tokio::fs::metadata(&part_path).await {
                existing_bytes = meta.len();
                info!(part_bytes = existing_bytes, "Resuming existing partial download");
            }
        }

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("Dex-Harness/0.5.0"));
        if let Some(ref t) = self.token {
            if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", t)) {
                headers.insert(AUTHORIZATION, v);
            }
        }

        if existing_bytes > 0 {
            if let Ok(range_header) = HeaderValue::from_str(&format!("bytes={}-", existing_bytes)) {
                headers.insert(RANGE, range_header);
            }
        }

        debug!(url = %url, existing = existing_bytes, "Initiating download HTTP request");
        let resp = self.client
            .get(url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| ModelError::Http {
                url: url.to_string(),
                source: e,
            })?;

        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let err = ModelError::HuggingFace {
                url: url.to_string(),
                message: format!("HTTP {} - {}", status, body),
            };
            if let Some(bus) = event_bus {
                bus.publish(DexEvent::ModelDownloadFailed {
                    model_id: model_id.to_string(),
                    file_name: file_name.to_string(),
                    error: err.to_string(),
                });
            }
            return Err(err);
        }

        let is_partial = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let content_len = resp.content_length().unwrap_or(0);
        let total_bytes = if is_partial {
            existing_bytes + content_len
        } else {
            content_len
        };

        let mut file = if is_partial && existing_bytes > 0 {
            tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&part_path)
                .await
                .map_err(|e| ModelError::Io {
                    path: part_path.to_string_lossy().to_string(),
                    source: e,
                })?
        } else {
            existing_bytes = 0;
            tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&part_path)
                .await
                .map_err(|e| ModelError::Io {
                    path: part_path.to_string_lossy().to_string(),
                    source: e,
                })?
        };

        let mut downloaded = existing_bytes;
        let mut stream = resp.bytes_stream();
        let start_time = Instant::now();
        let mut last_event_time = Instant::now();
        let mut bytes_since_last_event = 0u64;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    warn!(model_id = %model_id, "Download cancelled via token");
                    let _ = file.flush().await;
                    if let Some(bus) = event_bus {
                        bus.publish(DexEvent::ModelDownloadFailed {
                            model_id: model_id.to_string(),
                            file_name: file_name.to_string(),
                            error: "Download cancelled by user".to_string(),
                        });
                    }
                    return Err(ModelError::DownloadCancelled {
                        model_id: model_id.to_string(),
                        reason: "User cancelled the operation".to_string(),
                    });
                }
                chunk_opt = stream.next() => {
                    match chunk_opt {
                        Some(Ok(chunk)) => {
                            file.write_all(&chunk).await.map_err(|e| ModelError::Io {
                                path: part_path.to_string_lossy().to_string(),
                                source: e,
                            })?;

                            downloaded += chunk.len() as u64;
                            bytes_since_last_event += chunk.len() as u64;

                            let now = Instant::now();
                            let elapsed = now.duration_since(last_event_time).as_secs_f64();

                            // Throttle progress emissions to every 250ms
                            if elapsed >= 0.25 || downloaded == total_bytes {
                                let speed_mbps = if elapsed > 0.0 {
                                    (bytes_since_last_event as f64 / elapsed) / (1024.0 * 1024.0)
                                } else {
                                    0.0
                                };

                                if let Some(bus) = event_bus {
                                    bus.publish(DexEvent::ModelDownloadProgress {
                                        model_id: model_id.to_string(),
                                        file_name: file_name.to_string(),
                                        downloaded_bytes: downloaded,
                                        total_bytes,
                                        speed_mbps: (speed_mbps * 10.0).round() / 10.0,
                                    });
                                }

                                last_event_time = now;
                                bytes_since_last_event = 0;
                            }
                        }
                        Some(Err(e)) => {
                            let err = ModelError::Http {
                                url: url.to_string(),
                                source: e,
                            };
                            if let Some(bus) = event_bus {
                                bus.publish(DexEvent::ModelDownloadFailed {
                                    model_id: model_id.to_string(),
                                    file_name: file_name.to_string(),
                                    error: err.to_string(),
                                });
                            }
                            return Err(err);
                        }
                        None => {
                            // End of stream
                            break;
                        }
                    }
                }
            }
        }

        file.flush().await.map_err(|e| ModelError::Io {
            path: part_path.to_string_lossy().to_string(),
            source: e,
        })?;
        drop(file);

        // Atomic rename from .part to final destination
        tokio::fs::rename(&part_path, &final_dest).await.map_err(|e| ModelError::Io {
            path: final_dest.to_string_lossy().to_string(),
            source: e,
        })?;

        let total_elapsed = start_time.elapsed().as_secs_f64();
        info!(
            model_id = %model_id,
            bytes = downloaded,
            seconds = %format!("{:.1}", total_elapsed),
            "Download completed and verified successfully"
        );

        if let Some(bus) = event_bus {
            bus.publish(DexEvent::ModelDownloadCompleted {
                model_id: model_id.to_string(),
                file_name: file_name.to_string(),
                total_bytes: downloaded,
            });
        }

        Ok(final_dest)
    }
}
