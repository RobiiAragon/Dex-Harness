---
name: ux-qa-audit
description: Mandatory UX, QA, macOS .app bundle safety, Markdown rendering, and capability routing audit standard for Dex-Harness. Run before completing any desktop UI, runtime, or multimodal phase.
---

# Dex-Harness Permanent UX & QA Audit Standard

Every feature, phase, and release targeting `Dex.app` or any `harness-*` crate **must** pass all six invariants below before code is merged or bundled.

---

## 1. Zero Process-Relative (`cwd`) Writes

- On macOS, a bundled `.app` launched from Finder/Dock inherits `cwd = /`, which is a **read-only filesystem** (`os error 30: Read-only file system`).
- **Mandatory Rule:** No crate (`harness-core`, `harness-audio`, `harness-docs`, `harness-media`, `harness-3d`, `harness-computer`, `harness-models`, `harness-extensions`, `harness-desktop`) may create or write to a relative `.dex/...` path directly.
- Every artifact path must resolve against an explicit `storage_root` (active project `<project>/.dex` or `~/.dex` fallback via `harness_core::storage::resolve_dex_subpath`), and any caller-supplied absolute `output_path` must be honored without creating unused default directories under `/`.
- `active_workspace_dir` in `DesktopState` must never be `/`; if no project is active, default to `dirs::home_dir()` and persist internal Dex state under `~/.dex/`.

---

## 2. Zero Response Duplication in Agent Chat

- Streaming tokens (`DexEvent::ThoughtToken`) and completion payloads (`dex-goal-finished`) must consolidate into **one single assistant message** per turn.
- Never append a second `COMPLETED` card that repeats the streamed response text.

---

## 3. Zero Raw HTML or Unrendered Markdown

- Every assistant response in `Agent` and every model card in `Explore` must pass through the safe Markdown + sanitized HTML renderer (`renderSafeMarkdown`).
- Strip HTML comments (`<!-- ... -->`), decode common/numeric HTML entities (`&middot;`, `&nbsp;`, `&mdash;`, `&rarr;`, `&copy;`, `&#...;`), sanitize unsafe tags (`<script>`, `<iframe>`, `on*` handlers, `position: absolute`), and resolve relative Hugging Face image/link paths to `https://huggingface.co/<repo_id>/resolve/main/<path>`.
- Constrain inline badges/images so small shields or icons never stretch to 100% width (`max-width: 100%; width: auto; height: auto;`).

---

## 4. Frontier-Style Compact Activity Traces

- Reasoning (`<think>...</think>`) and tool calls must be grouped inside a **single collapsible turn accordion** above the assistant's final answer (e.g., `Working · 2 steps...` while active → auto-collapsed to `Worked for 4.2s · 3 actions` on completion).
- Individual tool rows expand on demand to inspect JSON arguments and output without cluttering the conversation feed.
- Chat sessions must auto-save globally in `~/.dex/sessions/` when no project is open, or in `<project>/.dex/sessions/` when a project is active, with auto-generated titles and rename/delete controls.

---

## 5. Honest Capability Routing & Pre-Flight Checks

- The Agent system prompt must carry a live **Host & Capabilities Manifest**: exact host chip (`sysctl -n machdep.cpu.brand_string`), macOS version (`sw_vers`), Unified Memory, active workspace, and explicit `READY` vs. `NOT INSTALLED` status for every multimodal engine (Blender, Apple Vision OCR, macOS TTS, Image Gen `mflux`/ComfyUI, Voice STT `mlx-whisper`/Apple Speech, 3D Gen TRELLIS/Hunyuan3D, Music/SFX Soundbox).
- Creative tools (`image_generate`, `video_generate`, `mesh_generate_3d`, `audio_generate_music`) must fail fast with an actionable `BackendNotInstalled` error (never loop-retry) naming the missing backend and 1-click install action, while guiding the agent to offer available installed alternatives (e.g., procedural 3D via Blender + `mesh_inspect`).

---

## 6. Real `.app` Bundle & `$PATH` Verification (`cwd = /`)

- All subprocess invocations from `Dex.app` must enrich `$PATH` with `/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin`, `~/.cargo/bin`, `~/.dex/runtimes/mlx-venv/bin`, and `/Applications/Blender.app/Contents/MacOS`.
- Every release build must pass:
  1. `cargo check --workspace --all-targets`
  2. `cargo clippy --workspace --all-targets -- -D warnings`
  3. `cargo test --workspace` (including tests simulating `cwd = /`)
  4. Apple WebKit `jsc` syntax + Markdown/HTML sanitizer unit assertions
  5. 0 `.unwrap()`/`.expect()` in production Rust, 0 `tokio::spawn` in `harness-desktop`, 0 emojis in UI
  6. Startup smoke-test of `Dex.app/Contents/MacOS/dex-desktop` executed explicitly from `(cd / && ...)` confirming zero `os error 30` errors.
