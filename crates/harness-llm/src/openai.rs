use std::time::Duration;
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use harness_core::{
    CompletionRequest, CompletionResponse, LlmProvider, ProviderError, Role, ToolCall,
};

#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    base_url: String,
    api_key: Option<String>,
    model: String,
    client: Client,
}

impl OpenAiProvider {
    pub const DEFAULT_BASE_URL: &'static str = "https://api.openai.com/v1";

    pub fn new(model: impl Into<String>) -> Self {
        Self {
            base_url: Self::DEFAULT_BASE_URL.to_string(),
            api_key: std::env::var("OPENAI_API_KEY").ok(),
            model: model.into(),
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ChatTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ChatFunction,
}

#[derive(Serialize, Deserialize)]
struct ChatFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: ChatCallFunction,
}

#[derive(Serialize, Deserialize)]
struct ChatCallFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    finish_reason: Option<String>,
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    #[instrument(skip(self, request), fields(model = %self.model))]
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let messages: Vec<ChatMessage> = request
            .messages
            .into_iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                ChatMessage {
                    role: role.to_string(),
                    content: m.content,
                    tool_calls: None,
                    tool_call_id: m.tool_call_id,
                }
            })
            .collect();

        let tools: Vec<ChatTool> = request
            .tools
            .into_iter()
            .map(|t| ChatTool {
                tool_type: "function".to_string(),
                function: ChatFunction {
                    name: t.name,
                    description: t.description,
                    parameters: t.parameters,
                },
            })
            .collect();

        let payload = ChatCompletionRequest {
            model: &self.model,
            messages,
            tools,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(ref key) = self.api_key {
            let auth_val = format!("Bearer {key}");
            if let Ok(hv) = HeaderValue::from_str(&auth_val) {
                headers.insert(AUTHORIZATION, hv);
            }
        }

        debug!(url = %url, "Sending request to OpenAI chat completions");

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ProviderError::Http(format!("Failed to reach API at {url}: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Custom {
                provider: "openai".to_string(),
                message: format!("HTTP {status}: {body}"),
            });
        }

        let resp: ChatCompletionResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::ParsingResponse(e.to_string()))?;

        let choice = resp.choices.into_iter().next().ok_or_else(|| {
            ProviderError::ParsingResponse("No choices returned in API response".to_string())
        })?;

        let tool_calls: Vec<ToolCall> = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| {
                let parsed_args = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::String(tc.function.arguments));
                ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments: parsed_args,
                }
            })
            .collect();

        Ok(CompletionResponse {
            content: choice.message.content,
            tool_calls,
            finish_reason: choice.finish_reason,
        })
    }
}
