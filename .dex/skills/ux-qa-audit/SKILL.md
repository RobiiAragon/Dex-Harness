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

- The Agent system prompt must carry a live **Host & Capabilities Manifest**: exact host chip (`sysctl -n machdep.cpu.brand_string`), macOS version (`sw_vers`), Unified Memory, active workspace, and explicit `READY` vs. `NOT INSTALLED` status for every multimodal engine (Blender, Apple Vision OCR, macOS TTS, Image Gen `mflux`/ComfyUI, Voice STT `mlx-whisper`/Apple Speech, 3D Gen TRELLIS/Hunyuan3D, Music/SFX `dex-audiogen` MusicGen/AudioLDM).
- Creative tools (`image_generate`, `video_generate`, `mesh_generate_3d`, `audio_generate_music`) must fail fast with an actionable `BackendNotInstalled` error (never loop-retry) naming the missing backend and 1-click install action, while guiding the agent to offer available installed alternatives (e.g., procedural 3D via Blender + `mesh_inspect`).

---

## 6. Real `.app` Bundle & `$PATH` Verification (`cwd = /`)

- All subprocess invocations from `Dex.app` must enrich `$PATH` with `/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin`, `~/.cargo/bin`, `~/.dex/runtimes/mlx-venv/bin`, and `/Applications/Blender.app/Contents/MacOS`.
- Never use `window.alert()` in `Dex.app` — Apple WKWebView drops `alert()` silently without a custom `UIDelegate`. Always present errors via visible inline toasts or actionable recovery banners with 1-click remediation buttons.

---

## 7. End-to-End Self-Verification Gate (MANDATORY Before Marking Any Phase Complete)

Unit tests alone are **NOT sufficient**. Every phase touching runtimes, multimodal engines, models, or desktop UI **must** pass ALL five real-host E2E gates before delivery:

1. **Real Subprocess & Flag Verification:** Execute `--help` and a real smoke invocation against installed local binaries (`mflux-generate`, `mlx_whisper`, `mlx_audio`, `blender`, `ffmpeg`, `llama-server`) confirming full `stdout` + `stderr` capture on failure and zero blind `exit status: 1` errors.
2. **Real Model Integrity Scan:** Execute `dex models list` against the user's live model directories (e.g., `~/.lmstudio/models`) confirming corrupted/partial downloads (missing shards in `model.safetensors.index.json`, `.part` files, 0-byte weights) are flagged `Incomplete` and blocked from loading, while intact models report `Valid`.
3. **Real Audio Pre-Processing & STT Roundtrip:** Generate or load a real spoken `.wav` (e.g., `dex audio speak "Hola mundo probando Dex en español"`), run Rust VAD silence trim + soft peak normalization + local STT with explicit `--language es`, and confirm the returned transcription matches the spoken Spanish (no language hallucinations).
4. **Live Visual & Interactive `Dex.app` Walkthrough (`DEX_QA_WALKTHROUGH=1` under `cwd = /`):**
   - Launch the freshly built `Dex.app/Contents/MacOS/dex-desktop` explicitly from `(cd / && DEX_QA_WALKTHROUGH=1 ...)`.
   - Automatically traverse every primary view (`Agent`, `Media Studio`, `Audio & Docs`, `3D & Game Studio`, `Models Hub` [`My Models` + `Explore`], `Extensions`, `Hardware Cookbook`).
   - **Mandatory Live-Configured State (Zero Error Toasts / Zero Unconfigured Views / Zero Empty Galleries in Screenshots):**
     - An active `Valid` local model must be mounted in Dex Native Runtime (`Loaded` in sidebar pill, `Active Loaded Model` banner, and `Dex MLX (:8088) 1 loaded` online — never `Select local model...`).
     - All 7 Apple Silicon runtimes (`mlx`, `llamacpp`, `mlx-whisper`, `mflux`, `mlx-audio`, `parakeet-mlx`, `faster-whisper`) must be installed and showing green `Ready` badges.
     - Actionable error toasts must be verified programmatically pre-flight and cleared before capturing showcase screenshots so no error/warning toast ever appears in any screenshot.
     - Every studio must display real populated outputs: active Agent turn with tool calls + GFM Markdown; Media Studio with populated Visual Gallery; Voice Studio with active cloned voice, playable audio cards, and real Apple Vision OCR extraction; 3D Studio with a multi-material Blender `.glb` mesh in WebGL2; Models Hub with `Valid` models sorted first + live `Explore` Hugging Face view; and Extensions with a live `Running` sidecar (`PID`) and zero stale records.
   - Verify `0` uncaught JS errors (`window.__DEX_QA_ERRORS__`), `0` `os error 30` / `Read-only file system` logs, presence of key interactive controls (`Browse`, language selectors, model pickers, actionable toasts), and visually inspect every captured screenshot via `read_file` to confirm zero layout clipping, zero error popups, and zero unconfigured states.
5. **Full Rust + WebKit Static & Build Gate:**
   - `cargo check --workspace --all-targets` (0 warnings)
   - `cargo clippy --workspace --all-targets -- -D warnings` (0 warnings)
   - `cargo test --workspace` (0 failures, including `cwd = /` tests)
   - Apple WebKit `jsc` syntax + unit checks on `crates/harness-desktop/ui/*.js`
   - Static audit: `0` production `.unwrap()`/`.expect()`, `0` `tokio::spawn` in `harness-desktop`, `0` UI emojis, `0` raw relative `.dex/...` paths
   - Rebuilt `Dex.app` bundle (`cargo-tauri build --bundles app --debug --no-sign`) verified under `cwd = /`.
