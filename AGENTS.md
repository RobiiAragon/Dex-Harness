# Engineering Guidelines & Conventions for Dex-Harness

This document establishes the architectural principles, coding standards, and safety invariants for **Dex-Harness**. Every contributor and autonomous coding agent working on this codebase must adhere strictly to these rules.

---

## 1. Zero Panics Policy (No `unwrap()` or `expect()`)

- **Prohibition:** Neither `.unwrap()` nor `.expect()` may be used in production code under any circumstances (`crates/harness-*`).
- **Allowed Exception:** Only permitted in unit tests (`#[cfg(test)]`) where failing fast on assertion setup is intentional.
- **Handling Infallible / Option Cases:**
  - Use `ok_or_else(|| CustomError::...)` to turn `Option<T>` into a descriptive `Result<T, E>`.
  - Use pattern matching (`if let`, `match`) with explicit fallback logic.
  - Return typed errors up the call stack via the `?` operator.

---

## 2. Error Handling Strategy

### Library Crates (`harness-core`, `harness-llm`, `harness-tools`, `harness-mcp`)
- All library crates must use **`thiserror`** for strongly typed, domain-specific error enums.
- Every public API must return `Result<T, ErrorType>`.
- Error messages must provide actionable context (e.g., path attempted, command executed, HTTP status code returned).
- Example:
  ```rust
  use thiserror::Error;

  #[derive(Error, Debug)]
  pub enum ToolError {
      #[error("I/O error while accessing path '{path}': {source}")]
      Io {
          path: String,
          #[source]
          source: std::io::Error,
      },
      #[error("Tool '{name}' execution timed out after {timeout_secs}s")]
      Timeout {
          name: String,
          timeout_secs: u64,
      },
      #[error("Invalid arguments for tool '{name}': {reason}")]
      InvalidArguments {
          name: String,
          reason: String,
      },
  }
  ```

### Binary Crate (`harness-cli`)
- The binary CLI layer may use **`anyhow::Result`** and `anyhow::Context` (`.with_context(|| ...)`) to enrich errors with user-facing contextual information before presentation.

---

## 3. Asynchronous Concurrency with `tokio`

1. **Non-blocking Rule:** Never execute synchronous, CPU-heavy, or blocking I/O inside an async function.
   - Use `tokio::fs` instead of `std::fs`.
   - Use `tokio::process::Command` instead of `std::process::Command`.
   - For CPU-bound tasks or non-async C-bindings, offload to `tokio::task::spawn_blocking`.
2. **Cancellation and Timeouts:**
   - Every external operation (network requests to LLMs, shell commands executed by the agent) **must** be bounded by a timeout using `tokio::time::timeout`.
3. **Structured Concurrency:**
   - Use `tokio::task::JoinSet` when spawning concurrent subagents to guarantee graceful shutdown and prevent orphaned tasks.
   - Use `tokio::sync::mpsc` or `tokio::sync::broadcast` for streaming events (tokens, stdout lines, status updates).

---

## 4. Tool Definitions & JSON Schema (`schemars`)

- All agent tools implement the `harness_core::Tool` trait.
- Tool input parameters must be defined as Rust structs deriving `serde::Deserialize` and `schemars::JsonSchema`.
- Schemas must be generated dynamically using `schemars::schema_for!` or `schemars::gen::SchemaGenerator` so that LLM providers receive up-to-date JSON Schema definitions.
- Tools must sanitize paths and arguments, preventing directory traversal vulnerabilities or unbounded shell execution when in restricted modes.

---

## 5. Tracing & Observability

- Use the `tracing` crate (`tracing::info!`, `tracing::warn!`, `tracing::error!`, `tracing::debug!`, `tracing::trace!`) instead of `println!` or `eprintln!` in library crates.
- CLI output intended directly for the user must be cleanly formatted and separated from debug telemetry.
- Instrument complex functions with `#[tracing::instrument(skip(...))]` to trace agent execution steps across asynchronous boundaries.

---

## 6. Code Style & Tooling Verification

Before submitting code, the following commands must execute cleanly with zero warnings:

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## 7. Permanent UX & QA Audit Standard (`Dex.app` & Multimodal Crates)

All desktop UI, runtime, and multimodal work **must** comply with the permanent audit checklist in [`.dex/skills/ux-qa-audit/SKILL.md`](.dex/skills/ux-qa-audit/SKILL.md):

- **Zero `cwd`-relative `.dex/...` writes:** macOS `.app` bundles launch with `cwd = /` (read-only, `os error 30`). Always resolve storage through `harness_core::storage` (`active_workspace_dir/.dex` or `~/.dex`) and honor explicit absolute `output_path` arguments.
- **Enriched subprocess `$PATH`:** Always include `/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin`, `~/.cargo/bin`, and `~/.dex/runtimes/mlx-venv/bin` when spawning external tools from `Dex.app`.
- **Zero chat duplication & full Markdown rendering:** Consolidate streamed tokens and final answers into one Markdown-rendered assistant block with a compact collapsible activity/`<think>` accordion (`Worked for Xs · N actions`).
- **Honest capability routing:** Inject a live Host & Capabilities Manifest into the system prompt and fail fast with `BackendNotInstalled` instead of letting the LLM loop on missing creative backends.
- **Always use `tauri::async_runtime::spawn`** inside `crates/harness-desktop` (never `tokio::spawn`) and run the `Dex.app` smoke-test explicitly from `(cd / && ...)`.
