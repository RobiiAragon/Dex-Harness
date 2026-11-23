use std::sync::Arc;
use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::error::CoreError;
use crate::memory::ConversationContext;
use crate::provider::{CompletionRequest, LlmProvider, Message};
use crate::tool::{ToolCall, ToolRegistry, ToolResult};

/// Represents a single step in the ReAct (Reasoning + Action) execution loop.
#[derive(Debug, Clone)]
pub struct AgentStep {
    pub iteration: usize,
    pub thought: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub tool_results: Vec<ToolResult>,
    pub final_answer: Option<String>,
    pub is_done: bool,
}

pub type StepObserver = Arc<dyn Fn(&AgentStep) + Send + Sync>;

/// Core trait representing an autonomous agent.
#[async_trait]
pub trait Agent: Send + Sync {
    /// Executes a high-level user goal to completion or until max iterations are reached.
    async fn run(&mut self, goal: &str) -> Result<String, CoreError>;

    /// Executes a single ReAct step (Reason -> Act -> Observe).
    async fn step(&mut self) -> Result<AgentStep, CoreError>;
}

/// A ReAct (Reasoning + Action) agent implementation that coordinates
/// LLM inference, tool execution, and conversation history.
pub struct ReActAgent {
    provider: Arc<dyn LlmProvider>,
    tools: ToolRegistry,
    context: ConversationContext,
    max_steps: usize,
    current_step: usize,
    observer: Option<StepObserver>,
}

impl ReActAgent {
    pub const DEFAULT_MAX_STEPS: usize = 25;

    pub fn new(provider: Arc<dyn LlmProvider>, tools: ToolRegistry) -> Self {
        let system_prompt = "\
You are Dex, a world-class autonomous software engineering and systems agent.
You have native access to tools allowing you to inspect files, execute shell commands, and interact with the environment.
Solve the user's objective thoroughly:
1. Break complex problems down methodically.
2. Formulate your reasoning and plan of action.
3. Call the appropriate tools with precise arguments.
4. Inspect the output of tool executions to verify results or diagnose failures.
5. Provide a clear, comprehensive final answer once the objective is fully satisfied.".to_string();

        Self {
            provider,
            tools,
            context: ConversationContext::new().with_system_prompt(system_prompt),
            max_steps: Self::DEFAULT_MAX_STEPS,
            current_step: 0,
            observer: None,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.context.set_system_prompt(prompt);
        self
    }

    pub fn with_max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    pub fn with_observer(mut self, observer: StepObserver) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn context(&self) -> &ConversationContext {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut ConversationContext {
        &mut self.context
    }
}

#[async_trait]
impl Agent for ReActAgent {
    async fn step(&mut self) -> Result<AgentStep, CoreError> {
        self.current_step += 1;
        let iteration = self.current_step;

        info!(iteration = iteration, max_steps = self.max_steps, "Executing ReAct step");

        let request = CompletionRequest {
            messages: self.context.assemble_messages(),
            tools: self.tools.definitions(),
            temperature: Some(0.2),
            max_tokens: None,
        };

        let response = self.provider.complete(request).await?;

        let thought = response.content.clone();
        let tool_calls = response.tool_calls.clone();

        // Check if agent concluded without requesting further tools
        if tool_calls.is_empty() {
            let answer = response.content.unwrap_or_default();
            debug!(iteration = iteration, "Agent finished: no tool calls requested");

            self.context.push(Message::assistant(Some(answer.clone()), None));

            let step = AgentStep {
                iteration,
                thought,
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
                final_answer: Some(answer),
                is_done: true,
            };

            if let Some(ref obs) = self.observer {
                obs(&step);
            }

            return Ok(step);
        }

        // Add assistant message containing the tool calls to conversation history
        self.context.push(Message::assistant(
            response.content.clone(),
            Some(tool_calls.clone()),
        ));

        // Execute each requested tool call
        let mut tool_results = Vec::with_capacity(tool_calls.len());
        for call in &tool_calls {
            let result = self.tools.execute(call).await;
            self.context.push(Message::tool_result(
                &result.call_id,
                &result.tool_name,
                &result.output,
            ));
            tool_results.push(result);
        }

        let step = AgentStep {
            iteration,
            thought,
            tool_calls,
            tool_results,
            final_answer: None,
            is_done: false,
        };

        if let Some(ref obs) = self.observer {
            obs(&step);
        }

        Ok(step)
    }

    async fn run(&mut self, goal: &str) -> Result<String, CoreError> {
        info!(goal = %goal, "Starting agent execution run");
        self.context.push(Message::user(goal));
        self.current_step = 0;

        while self.current_step < self.max_steps {
            let step = self.step().await?;
            if step.is_done {
                return Ok(step.final_answer.unwrap_or_default());
            }
        }

        warn!(max_steps = self.max_steps, "Maximum ReAct iterations reached without conclusion");
        Err(CoreError::MaxIterationsReached {
            max_steps: self.max_steps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use crate::error::ProviderError;
    use crate::provider::CompletionResponse;

    struct MockProvider {
        call_count: AtomicUsize,
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                // First call: call a tool
                Ok(CompletionResponse {
                    content: Some("I need to check the shell".to_string()),
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_string(),
                        name: "test_tool".to_string(),
                        arguments: serde_json::json!({}),
                    }],
                    finish_reason: None,
                })
            } else {
                // Second call: return final answer
                let last_message = req.messages.last().and_then(|m| m.content.as_deref()).unwrap_or("");
                Ok(CompletionResponse {
                    content: Some(format!("Goal achieved with tool output: {last_message}")),
                    tool_calls: Vec::new(),
                    finish_reason: Some("stop".to_string()),
                })
            }
        }
    }

    struct MockTool;

    #[async_trait]
    impl crate::tool::Tool for MockTool {
        fn name(&self) -> &str {
            "test_tool"
        }

        fn description(&self) -> &str {
            "Test tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn execute(&self, _args: serde_json::Value) -> Result<String, crate::error::ToolError> {
            Ok("Tool executed successfully".to_string())
        }
    }

    #[tokio::test]
    async fn test_react_agent_autonomous_cycle() {
        let provider = Arc::new(MockProvider {
            call_count: AtomicUsize::new(0),
        });
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(MockTool));

        let mut agent = ReActAgent::new(provider, tools);
        let result = agent.run("Run test").await.expect("Agent execution should succeed");

        assert!(result.contains("Goal achieved with tool output: Tool executed successfully"));
        assert_eq!(agent.context().messages().len(), 4); // User -> Assistant (call) -> Tool Result -> Assistant (final)
    }
}
