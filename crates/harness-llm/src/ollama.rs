use std::time::Duration;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use harness_core::{
    CompletionRequest, CompletionResponse, LlmProvider, ProviderError, Role, ToolCall,
};

#[derive(Debug, Clone)]
pub struct OllamaProvider {
    base_url: String,
    model: String,
    client: Client,
    timeout: Duration,
}

impl OllamaProvider {
    pub const DEFAULT_BASE_URL: &'static str = "http://localhost:11434";
    pub const DEFAULT_MODEL: &'static str = "qwen2.5:7b";

    pub fn new(model: impl Into<String>) -> Self {
        Self {
            base_url: Self::DEFAULT_BASE_URL.to_string(),
            model: model.into(),
            // No whole-request timeout: it would cut long streamed generations.
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
            timeout: Duration::from_secs(300),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

// Request and response DTOs for Ollama /api/chat
#[derive(Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: Vec<OllamaMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OllamaTool>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
}

#[derive(Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaToolCall>>,
    /// Reasoning text returned by thinking models (never sent back).
    #[serde(default, skip_serializing)]
    thinking: Option<String>,
    /// Name of the tool whose result this `tool` message carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct OllamaTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OllamaFunction,
}

#[derive(Serialize, Deserialize)]
struct OllamaFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct OllamaToolCall {
    #[serde(default)]
    id: Option<String>,
    function: OllamaCallFunction,
}

#[derive(Serialize, Deserialize)]
struct OllamaCallFunction {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    message: OllamaMessage,
    #[serde(default)]
    done_reason: Option<String>,
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    #[instrument(skip(self, request), fields(model = %self.model))]
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
        let is_streaming = request.token_callback.is_some();
        let token_callback = request.token_callback.clone();

        let ollama_messages: Vec<OllamaMessage> = request
            .messages
            .into_iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                let tool_calls = m.tool_calls.filter(|c| !c.is_empty()).map(|calls| {
                    calls
                        .into_iter()
                        .map(|c| OllamaToolCall {
                            id: Some(c.id),
                            function: OllamaCallFunction {
                                name: c.name,
                                arguments: c.arguments,
                            },
                        })
                        .collect()
                });
                let tool_name = if matches!(m.role, Role::Tool) { m.name } else { None };
                OllamaMessage {
                    role: role.to_string(),
                    content: m.content,
                    tool_calls,
                    thinking: None,
                    tool_name,
                }
            })
            .collect();

        let ollama_tools: Vec<OllamaTool> = request
            .tools
            .into_iter()
            .map(|t| OllamaTool {
                tool_type: "function".to_string(),
                function: OllamaFunction {
                    name: t.name,
                    description: t.description,
                    parameters: t.parameters,
                },
            })
            .collect();

        let options = request.temperature.map(|t| OllamaOptions {
            temperature: Some(t),
        });

        let payload = OllamaChatRequest {
            model: &self.model,
            messages: ollama_messages,
            tools: ollama_tools,
            stream: is_streaming,
            options,
        };

        debug!(url = %url, streaming = is_streaming, "Sending request to Ollama /api/chat");

        let response = tokio::time::timeout(self.timeout, self.client.post(&url).json(&payload).send())
            .await
            .map_err(|_| ProviderError::Custom {
                provider: "ollama".to_string(),
                message: format!(
                    "Timed out after {}s waiting for Ollama at {url} (model may still be loading).",
                    self.timeout.as_secs()
                ),
            })?
            .map_err(|e| ProviderError::Http(format!("Failed to reach Ollama at {url}: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Custom {
                provider: "ollama".to_string(),
                message: format!("HTTP {status}: {body}"),
            });
        }

        // Handle streaming response
        if is_streaming {
            let mut byte_stream = response.bytes_stream();
            let mut accumulated_content = String::new();
            let mut accumulated_tool_calls = Vec::new();
            let mut line_buffer: Vec<u8> = Vec::new();
            let mut finish_reason = None;
            let mut in_thinking = false;
            let idle = Duration::from_secs(120);

            loop {
                let next = tokio::time::timeout(idle, byte_stream.next())
                    .await
                    .map_err(|_| ProviderError::Custom {
                        provider: "ollama".to_string(),
                        message: format!("Stream stalled for {}s while reading from {url}.", idle.as_secs()),
                    })?;
                let Some(chunk_result) = next else { break };
                let chunk = chunk_result.map_err(|e| ProviderError::Http(e.to_string()))?;
                // Byte-level split keeps multi-byte UTF-8 characters intact across chunks.
                line_buffer.extend_from_slice(&chunk);

                while let Some(pos) = line_buffer.iter().position(|b| *b == b'\n') {
                    let raw_line: Vec<u8> = line_buffer.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&raw_line).trim().to_string();

                    if line.is_empty() {
                        continue;
                    }

                    if let Ok(chat_resp) = serde_json::from_str::<OllamaChatResponse>(&line) {
                        if let Some(ref thinking) = chat_resp.message.thinking {
                            if !thinking.is_empty() {
                                if let Some(ref cb) = token_callback {
                                    if !in_thinking {
                                        cb("<think>");
                                        in_thinking = true;
                                    }
                                    cb(thinking);
                                }
                            }
                        }
                        if let Some(ref delta) = chat_resp.message.content {
                            if !delta.is_empty() {
                                if let Some(ref cb) = token_callback {
                                    if in_thinking {
                                        cb("</think>");
                                        in_thinking = false;
                                    }
                                    cb(delta);
                                }
                                accumulated_content.push_str(delta);
                            }
                        }

                        if let Some(tcs) = chat_resp.message.tool_calls {
                            for tc in tcs {
                                accumulated_tool_calls.push(ToolCall {
                                    id: tc.id.unwrap_or_else(|| {
                                        format!("call_{}_{}", accumulated_tool_calls.len(), tc.function.name)
                                    }),
                                    name: tc.function.name,
                                    arguments: tc.function.arguments,
                                });
                            }
                        }

                        if chat_resp.done_reason.is_some() {
                            finish_reason = chat_resp.done_reason;
                        }
                    }
                }
            }

            if in_thinking {
                if let Some(ref cb) = token_callback {
                    cb("</think>");
                }
            }

            return Ok(CompletionResponse {
                content: if accumulated_content.is_empty() {
                    None
                } else {
                    Some(accumulated_content)
                },
                tool_calls: accumulated_tool_calls,
                finish_reason,
            });
        }

        // Non-streaming response
        let chat_resp: OllamaChatResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::ParsingResponse(e.to_string()))?;

        let tool_calls: Vec<ToolCall> = chat_resp
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(idx, tc)| ToolCall {
                id: tc.id.unwrap_or_else(|| format!("call_{idx}_{}", tc.function.name)),
                name: tc.function.name,
                arguments: tc.function.arguments,
            })
            .collect();

        Ok(CompletionResponse {
            content: chat_resp.message.content,
            tool_calls,
            finish_reason: chat_resp.done_reason,
        })
    }
}
