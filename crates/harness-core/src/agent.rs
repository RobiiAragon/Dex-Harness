use std::sync::Arc;
use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::compaction::{ContextCompactor, ToolOutputCompactor};
use crate::error::CoreError;
use crate::event::{DexEvent, EventBus};
use crate::hook::{AgentEvent, HookAction, HookRegistry};
use crate::memory::ConversationContext;
use crate::provider::{CompletionRequest, LlmProvider, Message, TokenCallback};
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
/// LLM inference, tool execution, conversation history, compaction, and hooks.
pub struct ReActAgent {
    provider: Arc<dyn LlmProvider>,
    tools: ToolRegistry,
    context: ConversationContext,
    hooks: HookRegistry,
    compactor: ContextCompactor,
    output_compactor: ToolOutputCompactor,
    max_steps: usize,
    current_step: usize,
    observer: Option<StepObserver>,
    token_callback: Option<TokenCallback>,
    event_bus: Option<EventBus>,
}

impl ReActAgent {
    pub const DEFAULT_MAX_STEPS: usize = 25;

    pub fn new(provider: Arc<dyn LlmProvider>, tools: ToolRegistry) -> Self {
        let system_prompt = "\
You are Dex, a world-class autonomous software engineering and systems agent.
You have native access to tools allowing you to inspect files, execute shell commands, edit files surgically, and interact with the environment.
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
            hooks: HookRegistry::new(),
            compactor: ContextCompactor::default(),
            output_compactor: ToolOutputCompactor::default(),
            max_steps: Self::DEFAULT_MAX_STEPS,
            current_step: 0,
            observer: None,
            token_callback: None,
            event_bus: None,
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

    pub fn with_token_callback(mut self, callback: TokenCallback) -> Self {
        self.token_callback = Some(callback);
        self
    }

    pub fn with_event_bus(mut self, event_bus: EventBus) -> Self {
        self.event_bus = Some(event_bus);
        self
    }

    pub fn with_hooks(mut self, hooks: HookRegistry) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn with_compactor(mut self, compactor: ContextCompactor) -> Self {
        self.compactor = compactor;
        self
    }

    pub fn with_output_compactor(mut self, output_compactor: ToolOutputCompactor) -> Self {
        self.output_compactor = output_compactor;
        self
    }

    /// Tool definitions currently exposed to the model (core + activated deferred tools).
    pub fn tool_definitions(&self) -> Vec<crate::tool::ToolDefinition> {
        self.tools.active_definitions()
    }

    pub fn context(&self) -> &ConversationContext {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut ConversationContext {
        &mut self.context
    }

    /// Closes out any assistant tool calls that never received a tool result (e.g. the run was
    /// cancelled with Ctrl-C mid-tool), so the next request is accepted by strict
    /// OpenAI-compatible servers. Returns how many placeholder results were appended.
    pub fn repair_history(&mut self) -> usize {
        let messages = self.context.messages();
        let Some(last_call_idx) = messages
            .iter()
            .rposition(|m| m.tool_calls.as_ref().is_some_and(|c| !c.is_empty()))
        else {
            return 0;
        };
        let answered: std::collections::HashSet<String> = messages[last_call_idx + 1..]
            .iter()
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        let missing: Vec<(String, String)> = messages[last_call_idx]
            .tool_calls
            .iter()
            .flatten()
            .filter(|c| !answered.contains(&c.id))
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect();
        for (id, name) in &missing {
            self.context
                .push(Message::tool_result(id, name, "Cancelled by the user before completion."));
        }
        missing.len()
    }

    /// Clears the conversation (keeping the system prompt) for a fresh session.
    pub fn reset_conversation(&mut self) {
        self.context.clear_history();
        self.current_step = 0;
    }

    /// Executes a user goal with optional multimodal image data URLs attached to the user turn.
    pub async fn run_with_images(
        &mut self,
        goal: &str,
        image_data_urls: Vec<String>,
    ) -> Result<String, CoreError> {
        info!(goal = %goal, images = image_data_urls.len(), "Starting multimodal agent execution run");
        if image_data_urls.is_empty() {
            self.context.push(Message::user(goal));
        } else {
            self.context.push(Message::user_with_images(goal, image_data_urls));
        }
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

#[async_trait]
impl Agent for ReActAgent {
    async fn step(&mut self) -> Result<AgentStep, CoreError> {
        self.current_step += 1;
        let iteration = self.current_step;

        info!(iteration = iteration, max_steps = self.max_steps, "Executing ReAct step");

        // Automatically compact context if token threshold is exceeded
        if self.compactor.compact(&mut self.context) {
            if let Some(ref bus) = self.event_bus {
                let estimated = ContextCompactor::estimate_tokens(self.context.messages());
                bus.publish(DexEvent::ContextCompacted { estimated_tokens: estimated });
            }
        }

        // Bridge token callback to EventBus if token_callback wasn't explicitly set
        let effective_token_cb: Option<TokenCallback> = match (&self.token_callback, &self.event_bus) {
            (Some(cb), Some(bus)) => {
                let cb_clone = cb.clone();
                let bus_clone = bus.clone();
                Some(Arc::new(move |token: &str| {
                    bus_clone.publish(DexEvent::ThoughtToken { token: token.to_string() });
                    cb_clone(token);
                }))
            }
            (Some(cb), None) => Some(cb.clone()),
            (None, Some(bus)) => {
                let bus_clone = bus.clone();
                Some(Arc::new(move |token: &str| {
                    bus_clone.publish(DexEvent::ThoughtToken { token: token.to_string() });
                }))
            }
            (None, None) => None,
        };

        let request = CompletionRequest {
            messages: self.context.assemble_messages(),
            tools: self.tools.active_definitions(),
            temperature: Some(0.2),
            max_tokens: None,
            token_callback: effective_token_cb,
        };

        let response = match self.provider.complete(request).await {
            Ok(resp) => resp,
            Err(e) => {
                let err_msg = e.to_string();
                self.hooks.trigger(&AgentEvent::OnError { error: err_msg.clone() }).await?;
                return Err(e.into());
            }
        };

        let thought = response.content.clone();
        let tool_calls = response.tool_calls.clone();

        // Check if agent concluded without requesting further tools
        if tool_calls.is_empty() {
            if response.finish_reason.as_deref() == Some("length") {
                tracing::warn!(
                    iteration = iteration,
                    "Model stopped at its output token limit; the answer is truncated"
                );
            }
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

        // Execute each requested tool call, running lifecycle hooks
        let mut tool_results = Vec::with_capacity(tool_calls.len());
        for (index, mut call) in tool_calls.clone().into_iter().enumerate() {
            // Trigger PreToolUse hook
            let pre_event = AgentEvent::PreToolUse {
                tool_name: call.name.clone(),
                arguments: call.arguments.clone(),
            };

            match self.hooks.trigger(&pre_event).await? {
                HookAction::Continue => {}
                HookAction::Halt(reason) => {
                    if let Some(ref bus) = self.event_bus {
                        bus.publish(DexEvent::SecurityBlocked {
                            tool_name: call.name.clone(),
                            reason: reason.clone(),
                        });
                    }
                    // Every assistant tool call must be answered by a tool message, otherwise
                    // OpenAI-compatible servers reject the *next* turn. Close them all out.
                    for pending in &tool_calls[index..] {
                        self.context.push(Message::tool_result(
                            &pending.id,
                            &pending.name,
                            format!("Not executed: {reason}"),
                        ));
                    }
                    return Err(CoreError::AgentHalted { reason });
                }
                HookAction::Deny(reason) => {
                    if let Some(ref bus) = self.event_bus {
                        bus.publish(DexEvent::SecurityBlocked {
                            tool_name: call.name.clone(),
                            reason: reason.clone(),
                        });
                    }
                    let result = ToolResult::error(&call.id, &call.name, format!("Denied: {reason}"));
                    // Let observers (UI, audit hooks) see the denial like any other result.
                    self.hooks
                        .trigger(&AgentEvent::PostToolUse {
                            tool_name: call.name.clone(),
                            result: result.clone(),
                        })
                        .await?;
                    self.context.push(Message::tool_result(
                        &result.call_id,
                        &result.tool_name,
                        &result.output,
                    ));
                    tool_results.push(result);
                    continue;
                }
                HookAction::ModifyArguments(new_args) => {
                    call.arguments = new_args;
                }
            }

            if let Some(ref bus) = self.event_bus {
                bus.publish(DexEvent::ToolStarted {
                    tool_name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
            }

            let mut result = self.tools.execute(&call).await;

            // Guardrail: spill oversized tool outputs (e.g. bulky MCP responses) to disk
            let (effective_output, _spilled_path) = self
                .output_compactor
                .compact_if_needed(&result.tool_name, &result.call_id, &result.output)
                .await;
            result.output = effective_output;

            if let Some(ref bus) = self.event_bus {
                bus.publish(DexEvent::ToolFinished {
                    tool_name: call.name.clone(),
                    output: result.output.clone(),
                    is_error: result.is_error,
                });
            }

            // Trigger PostToolUse hook
            let post_event = AgentEvent::PostToolUse {
                tool_name: call.name.clone(),
                result: result.clone(),
            };
            self.hooks.trigger(&post_event).await?;

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

    struct HaltHook(HookAction);

    #[async_trait]
    impl crate::hook::Hook for HaltHook {
        async fn on_event(&self, event: &AgentEvent) -> Result<HookAction, CoreError> {
            match event {
                AgentEvent::PreToolUse { .. } => Ok(self.0.clone()),
                _ => Ok(HookAction::Continue),
            }
        }
    }

    fn every_tool_call_answered(agent: &ReActAgent) -> bool {
        let msgs = agent.context().messages();
        msgs.iter().enumerate().all(|(i, m)| {
            m.tool_calls.iter().flatten().all(|c| {
                msgs[i + 1..]
                    .iter()
                    .any(|r| r.tool_call_id.as_deref() == Some(c.id.as_str()))
            })
        })
    }

    #[tokio::test]
    async fn halt_keeps_history_valid_for_next_turn() {
        let provider = Arc::new(MockProvider { call_count: AtomicUsize::new(0) });
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(MockTool));
        let mut hooks = HookRegistry::new();
        hooks.register(Arc::new(HaltHook(HookAction::Halt("user said no".into()))));
        let mut agent = ReActAgent::new(provider, tools).with_hooks(hooks);

        let err = agent.run("Run test").await;
        assert!(matches!(err, Err(CoreError::AgentHalted { .. })));
        assert!(every_tool_call_answered(&agent));
    }

    #[tokio::test]
    async fn deny_feeds_reason_back_and_continues() {
        let provider = Arc::new(MockProvider { call_count: AtomicUsize::new(0) });
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(MockTool));
        let mut hooks = HookRegistry::new();
        hooks.register(Arc::new(HaltHook(HookAction::Deny("plan mode".into()))));
        let mut agent = ReActAgent::new(provider, tools).with_hooks(hooks);

        let answer = agent.run("Run test").await.expect("deny should not abort the run");
        assert!(answer.contains("Denied: plan mode"), "got: {answer}");
        assert!(every_tool_call_answered(&agent));
    }

    #[test]
    fn repair_history_closes_dangling_tool_calls() {
        let provider = Arc::new(MockProvider { call_count: AtomicUsize::new(0) });
        let mut agent = ReActAgent::new(provider, ToolRegistry::new());
        agent.context_mut().push(Message::user("hi"));
        agent.context_mut().push(Message::assistant(
            None,
            Some(vec![ToolCall {
                id: "c1".into(),
                name: "test_tool".into(),
                arguments: serde_json::json!({}),
            }]),
        ));
        assert_eq!(agent.repair_history(), 1);
        assert_eq!(agent.repair_history(), 0);
        assert!(every_tool_call_answered(&agent));
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
