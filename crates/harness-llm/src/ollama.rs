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
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| Client::new()),
            timeout: Duration::from_secs(120),
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
                OllamaMessage {
                    role: role.to_string(),
                    content: m.content,
                    tool_calls: None,
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

        let response = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
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
            let mut line_buffer = String::new();
            let mut finish_reason = None;

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk = chunk_result.map_err(|e| ProviderError::Http(e.to_string()))?;
                let text = String::from_utf8_lossy(&chunk);
                line_buffer.push_str(&text);

                while let Some(pos) = line_buffer.find('\n') {
                    let line = line_buffer[..pos].trim().to_string();
                    line_buffer = line_buffer[pos + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }

                    if let Ok(chat_resp) = serde_json::from_str::<OllamaChatResponse>(&line) {
                        if let Some(ref delta) = chat_resp.message.content {
                            if !delta.is_empty() {
                                if let Some(ref cb) = token_callback {
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
