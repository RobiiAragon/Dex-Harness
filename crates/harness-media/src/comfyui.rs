use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use harness_core::{DexEvent, EventBus};
use serde::Deserialize;
use tracing::{debug, info};

use crate::error::MediaError;

pub const DEFAULT_WORKFLOWS_DIR: &str = ".dex/workflows";
const COMFYUI_POLL_TIMEOUT_SECS: u64 = 300;

pub const DEFAULT_TXT2IMG_WORKFLOW_JSON: &str = r#"{
  "3": {
    "class_type": "KSampler",
    "inputs": {
      "seed": "{{SEED}}",
      "steps": "{{STEPS}}",
      "cfg": 7.0,
      "sampler_name": "euler",
      "scheduler": "normal",
      "denoise": 1.0,
      "model": ["4", 0],
      "positive": ["6", 0],
      "negative": ["7", 0],
      "latent_image": ["5", 0]
    }
  },
  "4": {
    "class_type": "CheckpointLoaderSimple",
    "inputs": {
      "ckpt_name": "v1-5-pruned-emaonly.safetensors"
    }
  },
  "5": {
    "class_type": "EmptyLatentImage",
    "inputs": {
      "width": "{{WIDTH}}",
      "height": "{{HEIGHT}}",
      "batch_size": 1
    }
  },
  "6": {
    "class_type": "CLIPTextEncode",
    "inputs": {
      "text": "{{PROMPT}}",
      "clip": ["4", 1]
    }
  },
  "7": {
    "class_type": "CLIPTextEncode",
    "inputs": {
      "text": "{{NEGATIVE_PROMPT}}",
      "clip": ["4", 1]
    }
  },
  "8": {
    "class_type": "VAEDecode",
    "inputs": {
      "samples": ["3", 0],
      "vae": ["4", 2]
    }
  },
  "9": {
    "class_type": "SaveImage",
    "inputs": {
      "filename_prefix": "dex_comfy",
      "images": ["8", 0]
    }
  }
}"#;

pub const DEFAULT_UPSCALE_WORKFLOW_JSON: &str = r#"{
  "1": {
    "class_type": "LoadImage",
    "inputs": {
      "image": "{{IMAGE_PATH}}"
    }
  },
  "2": {
    "class_type": "ImageScaleBy",
    "inputs": {
      "image": ["1", 0],
      "upscale_method": "lanczos",
      "scale_by": "{{SCALE}}"
    }
  },
  "3": {
    "class_type": "SaveImage",
    "inputs": {
      "filename_prefix": "dex_upscale",
      "images": ["2", 0]
    }
  }
}"#;

pub const DEFAULT_VIDEO_WORKFLOW_JSON: &str = r#"{
  "1": {
    "class_type": "CLIPTextEncode",
    "inputs": {
      "text": "{{PROMPT}}"
    }
  },
  "2": {
    "class_type": "EmptyLatentVideo",
    "inputs": {
      "width": "{{WIDTH}}",
      "height": "{{HEIGHT}}",
      "length": 49,
      "batch_size": 1
    }
  },
  "3": {
    "class_type": "KSampler",
    "inputs": {
      "seed": "{{SEED}}",
      "steps": "{{STEPS}}",
      "cfg": 6.0,
      "positive": ["1", 0],
      "latent_image": ["2", 0]
    }
  }
}"#;

/// Ensures `.dex/workflows/` exists with default templates if none are present.
pub async fn ensure_default_workflows(dir: &Path) -> Result<(), MediaError> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| MediaError::Io {
            path: dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let defaults = [
        ("txt2img_default.json", DEFAULT_TXT2IMG_WORKFLOW_JSON),
        ("upscale_default.json", DEFAULT_UPSCALE_WORKFLOW_JSON),
        ("video_default.json", DEFAULT_VIDEO_WORKFLOW_JSON),
    ];

    for (name, content) in defaults {
        let p = dir.join(name);
        if !tokio::fs::try_exists(&p).await.unwrap_or(false) {
            tokio::fs::write(&p, content)
                .await
                .map_err(|e| MediaError::Io {
                    path: p.to_string_lossy().to_string(),
                    source: e,
                })?;
        }
    }

    Ok(())
}

/// Lists all JSON workflow template filenames inside `.dex/workflows/`.
pub async fn list_workflows(dir: &Path) -> Result<Vec<String>, MediaError> {
    ensure_default_workflows(dir).await?;
    let mut rd = tokio::fs::read_dir(dir).await.map_err(|e| MediaError::Io {
        path: dir.to_string_lossy().to_string(),
        source: e,
    })?;

    let mut names = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Loads a workflow JSON file by name (relative to `.dex/workflows/`) or absolute path.
pub async fn load_workflow_template(
    workflows_dir: &Path,
    workflow_name: &str,
) -> Result<String, MediaError> {
    ensure_default_workflows(workflows_dir).await?;
    let direct = PathBuf::from(workflow_name);
    let candidate = if direct.is_absolute() || tokio::fs::try_exists(&direct).await.unwrap_or(false) {
        direct
    } else {
        workflows_dir.join(workflow_name)
    };

    tokio::fs::read_to_string(&candidate)
        .await
        .map_err(|e| MediaError::Io {
            path: candidate.to_string_lossy().to_string(),
            source: e,
        })
}

/// Substitutes string placeholders (JSON-escaped) and numeric placeholders into a ComfyUI workflow template.
pub fn inject_workflow_placeholders(
    template: &str,
    string_vars: &HashMap<&str, String>,
    numeric_vars: &HashMap<&str, u64>,
) -> Result<serde_json::Value, MediaError> {
    let mut rendered = template.to_string();

    for (key, val) in numeric_vars {
        let quoted_token = format!("\"{{{{{key}}}}}\"");
        let raw_token = format!("{{{{{key}}}}}");
        let num_str = val.to_string();
        rendered = rendered.replace(&quoted_token, &num_str);
        rendered = rendered.replace(&raw_token, &num_str);
    }

    for (key, val) in string_vars {
        let raw_token = format!("{{{{{key}}}}}");
        // Serialize as JSON string and trim surrounding quotes so embedded quotes/newlines are safely escaped
        let escaped = serde_json::to_string(val).map_err(|e| MediaError::Json {
            context: format!("escape workflow placeholder {key}"),
            source: e,
        })?;
        let inner = escaped
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(&escaped);
        rendered = rendered.replace(&raw_token, inner);
    }

    serde_json::from_str::<serde_json::Value>(&rendered).map_err(|e| MediaError::Json {
        context: "parse injected ComfyUI workflow JSON".to_string(),
        source: e,
    })
}

#[derive(Debug, Deserialize)]
struct ComfyPromptResponse {
    prompt_id: String,
}

#[derive(Debug, Deserialize)]
struct ComfyOutputFile {
    filename: String,
    #[serde(default)]
    subfolder: String,
    #[serde(default, rename = "type")]
    file_type: String,
}

/// Asynchronous HTTP client for ComfyUI (`/prompt`, `/history/{id}`, `/view`).
pub struct ComfyUiClient {
    client: reqwest::Client,
    base_url: String,
}

impl Default for ComfyUiClient {
    fn default() -> Self {
        let url = std::env::var("DEX_COMFYUI_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:8188".to_string());
        Self::new(url)
    }
}

impl ComfyUiClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let url = base_url.into().trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            base_url: url,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn is_reachable(&self) -> bool {
        let probe = reqwest::Client::builder()
            .timeout(Duration::from_millis(400))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let url = format!("{}/system_stats", self.base_url);
        probe.get(&url).send().await.is_ok()
    }

    pub async fn queue_prompt(
        &self,
        workflow: serde_json::Value,
        client_id: &str,
    ) -> Result<String, MediaError> {
        let url = format!("{}/prompt", self.base_url);
        let body = serde_json::json!({
            "prompt": workflow,
            "client_id": client_id,
        });

        debug!(url = %url, client_id = %client_id, "Queuing ComfyUI workflow prompt");
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| MediaError::Http {
                url: url.clone(),
                source: e,
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            return Err(MediaError::ComfyUi {
                message: format!("POST /prompt returned HTTP {status}: {err_text}"),
            });
        }

        let parsed: ComfyPromptResponse = resp.json().await.map_err(|e| MediaError::Http {
            url,
            source: e,
        })?;
        Ok(parsed.prompt_id)
    }

    pub async fn poll_history_and_download(
        &self,
        prompt_id: &str,
        output_dir: &Path,
        task_id: &str,
        kind: &str,
        total_steps: u32,
        event_bus: Option<&EventBus>,
    ) -> Result<Vec<PathBuf>, MediaError> {
        tokio::fs::create_dir_all(output_dir)
            .await
            .map_err(|e| MediaError::Io {
                path: output_dir.to_string_lossy().to_string(),
                source: e,
            })?;

        let history_url = format!("{}/history/{}", self.base_url, prompt_id);
        let start = std::time::Instant::now();
        let mut poll_tick: u32 = 0;

        loop {
            if start.elapsed() >= Duration::from_secs(COMFYUI_POLL_TIMEOUT_SECS) {
                return Err(MediaError::Timeout {
                    operation: format!("ComfyUI prompt {prompt_id}"),
                    timeout_secs: COMFYUI_POLL_TIMEOUT_SECS,
                });
            }

            poll_tick = poll_tick.saturating_add(1);
            if let Some(bus) = event_bus {
                let pseudo_step = poll_tick.min(total_steps.saturating_sub(1).max(1));
                bus.publish(DexEvent::MediaGenerationProgress {
                    task_id: task_id.to_string(),
                    kind: kind.to_string(),
                    step: pseudo_step,
                    total_steps: total_steps.max(1),
                    message: format!("Waiting for ComfyUI prompt {prompt_id}..."),
                });
            }

            let resp = self
                .client
                .get(&history_url)
                .send()
                .await
                .map_err(|e| MediaError::Http {
                    url: history_url.clone(),
                    source: e,
                })?;

            if resp.status().is_success() {
                let json: serde_json::Value = resp.json().await.map_err(|e| MediaError::Http {
                    url: history_url.clone(),
                    source: e,
                })?;

                if let Some(entry) = json.get(prompt_id) {
                    let files = extract_output_files(entry);
                    if !files.is_empty() {
                        let mut saved_paths = Vec::with_capacity(files.len());
                        for file in files {
                            let saved = self.download_output_file(&file, output_dir).await?;
                            saved_paths.push(saved);
                        }
                        if let Some(bus) = event_bus {
                            bus.publish(DexEvent::MediaGenerationProgress {
                                task_id: task_id.to_string(),
                                kind: kind.to_string(),
                                step: total_steps.max(1),
                                total_steps: total_steps.max(1),
                                message: "ComfyUI output downloaded".to_string(),
                            });
                        }
                        info!(prompt_id = %prompt_id, files = saved_paths.len(), "Downloaded ComfyUI media");
                        return Ok(saved_paths);
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(750)).await;
        }
    }

    async fn download_output_file(
        &self,
        file: &ComfyOutputFile,
        output_dir: &Path,
    ) -> Result<PathBuf, MediaError> {
        let view_url = format!(
            "{}/view?filename={}&subfolder={}&type={}",
            self.base_url,
            url_encode_component(&file.filename),
            url_encode_component(&file.subfolder),
            url_encode_component(if file.file_type.is_empty() {
                "output"
            } else {
                &file.file_type
            })
        );

        let resp = self
            .client
            .get(&view_url)
            .send()
            .await
            .map_err(|e| MediaError::Http {
                url: view_url.clone(),
                source: e,
            })?;

        if !resp.status().is_success() {
            return Err(MediaError::ComfyUi {
                message: format!(
                    "Failed to download '{}' from ComfyUI /view (HTTP {})",
                    file.filename,
                    resp.status()
                ),
            });
        }

        let bytes = resp.bytes().await.map_err(|e| MediaError::Http {
            url: view_url,
            source: e,
        })?;

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let dest = output_dir.join(format!("comfy_{ts}_{}", file.filename));
        tokio::fs::write(&dest, &bytes)
            .await
            .map_err(|e| MediaError::Io {
                path: dest.to_string_lossy().to_string(),
                source: e,
            })?;

        Ok(dest)
    }
}

fn extract_output_files(history_entry: &serde_json::Value) -> Vec<ComfyOutputFile> {
    let mut found = Vec::new();
    let Some(outputs) = history_entry.get("outputs").and_then(|v| v.as_object()) else {
        return found;
    };

    for node_out in outputs.values() {
        for key in ["images", "gifs", "videos"] {
            if let Some(arr) = node_out.get(key).and_then(|v| v.as_array()) {
                for item in arr {
                    if let Ok(parsed) = serde_json::from_value::<ComfyOutputFile>(item.clone()) {
                        found.push(parsed);
                    }
                }
            }
        }
    }
    found
}

fn url_encode_component(input: &str) -> String {
    let mut out = String::new();
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_comfyui_workflow_placeholder_injection_handles_escaping_and_numbers() {
        let mut str_vars = HashMap::new();
        str_vars.insert(
            "PROMPT",
            "Cyberpunk \"Tokyo\" street\nneon rain, ultra detailed".to_string(),
        );
        str_vars.insert("NEGATIVE_PROMPT", "blurry, lowres".to_string());

        let mut num_vars = HashMap::new();
        num_vars.insert("SEED", 424242);
        num_vars.insert("WIDTH", 1024);
        num_vars.insert("HEIGHT", 768);
        num_vars.insert("STEPS", 25);

        let injected = inject_workflow_placeholders(
            DEFAULT_TXT2IMG_WORKFLOW_JSON,
            &str_vars,
            &num_vars,
        )
        .expect("workflow JSON must remain valid after placeholder injection");

        assert_eq!(injected["3"]["inputs"]["seed"], 424242);
        assert_eq!(injected["3"]["inputs"]["steps"], 25);
        assert_eq!(injected["5"]["inputs"]["width"], 1024);
        assert_eq!(injected["5"]["inputs"]["height"], 768);
        assert_eq!(
            injected["6"]["inputs"]["text"],
            "Cyberpunk \"Tokyo\" street\nneon rain, ultra detailed"
        );
        assert_eq!(injected["7"]["inputs"]["text"], "blurry, lowres");
    }

    #[tokio::test]
    async fn test_ensure_and_list_default_workflows() {
        let temp_dir =
            std::env::temp_dir().join(format!("dex_wf_test_{}", std::process::id()));
        let workflows = list_workflows(&temp_dir).await.expect("list workflows");
        assert!(workflows.contains(&"txt2img_default.json".to_string()));
        assert!(workflows.contains(&"upscale_default.json".to_string()));
        assert!(workflows.contains(&"video_default.json".to_string()));

        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }
}
