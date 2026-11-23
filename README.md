# Dex-Harness 🦀🤖

> **High-Performance Autonomous Agent Harness in Rust**  
> Empowers open-weights and commercial language models (Ollama, vLLM, llama.cpp, OpenAI, Anthropic) with native execution capabilities, computer vision, browser/desktop control, bidirectional MCP interoperability, and real-time observability.

---

## ⚡ Vision & Architectural Pillars

**Dex-Harness** is an enterprise-grade agent execution harness designed from the ground up in Rust for ultra-low latency, memory safety, and zero-panic resilience. Inspired by systems like Goose, Claude Code, and Odysseus, it bridges the gap between raw LLM inference, browser/desktop perception, and deep operating system interaction.

1. **Core & Inference Engine (`harness-core`, `harness-llm`):**
   - Universal multi-provider client supporting Ollama, OpenAI-compatible backends (vLLM, llama.cpp, LocalAI), and cloud APIs.
   - Built-in ReAct reasoning engine, real-time token streaming, dynamic tool calling with JSON Schema generation (`schemars`), and automated token compaction (`ContextCompactor`).
2. **Real-Time EventBus (`harness-core`):**
   - Non-blocking pub/sub message bus (`tokio::sync::broadcast`) broadcasting telemetry (`ThoughtToken`, `ToolStarted`, `ToolFinished`, `SubagentSpawned`, `TaskUpdated`, `ContextCompacted`, `SecurityBlocked`).
3. **Computer Use & Perception (`harness-computer`):**
   - **Browser CDP Automation (`chromiumoxide`):** Headless and visible Chrome automation (`browser_navigate`, `browser_extract_dom`, `browser_click`, `browser_type`, `browser_screenshot`).
   - **Desktop Automation (`xcap` + `enigo`):** Primary monitor screenshot capture (`desktop_screenshot`) and mouse/keyboard simulation (`desktop_mouse`, `desktop_keyboard`) with coordinate validation.
4. **Bidirectional Model Context Protocol (`harness-mcp`):**
   - Exposes any Dex `ToolRegistry` as a compliant MCP Server over `stdio` (`dex mcp-serve`) for instant integration with Claude Desktop, Cursor, or Zed.
5. **Real-time Interfaces: TUI & Embedded Web Dashboard (`harness-ui`):**
   - **TUI Dashboard (`dex tui`):** Terminal dashboard (`ratatui` + `crossterm`) showing active subagents, live thought stream, taskboard, and context window gauge.
   - **Web Dashboard (`dex web`):** Embedded web server (`axum` + WebSockets) with dark-mode dashboard, live telemetry, and screenshot gallery.
6. **Hardware Cookbook & Autonomous Scheduler (`harness-core`, `harness-cli`):**
   - **Hardware Profiling (`dex cookbook`):** Scans host RAM, CPU cores, and Apple Silicon unified memory to calculate recommended local model tiers and context sizes.
   - **Autonomous Cron Scheduler (`dex schedule` & `dex daemon`):** Manages recurring agent tasks in `.dex/schedules.json` executed by a background daemon.

---

## 📦 Workspace Architecture

```text
crates/
├── harness-core/        # EventBus, ReAct Agent, Memory, TaskBoard, Subagents, Skills, Hooks, Cookbook, Scheduler
├── harness-llm/         # LLM provider adapters (Ollama, OpenAI-compatible, streaming)
├── harness-tools/       # Native tools (filesystem, surgical edit, search, git worktrees, PTY shell)
├── harness-computer/    # Browser automation (CDP) and desktop perception/mouse/keyboard control
├── harness-mcp/         # Bidirectional Model Context Protocol client & stdio server
├── harness-models/      # Local Model Hub: Hugging Face GGUF client, resumable downloader, Ollama bridge, local importer
├── harness-extensions/  # Extensions Hub: MCP + Sidecar supervisor with idle RAM auto-unload & deferred tool bridge
├── harness-audio/       # Native microphone capture (cpal/hound), Whisper STT, TTS (sidecar + macOS say), Music & SFX
├── harness-docs/        # Native Apple Vision OCR (macOS) / Tesseract fallback & Docling / pure-Rust PDF parser
├── harness-media/       # Visual Studio: MLX/ComfyUI/HTTP image & video gen, Lanczos3 upscale, pure-Rust image ops, FFmpeg timeline editor
├── harness-3d/          # 3D & Game Studio: pure-Rust glTF/OBJ inspector, image-to-3D sidecars, headless Blender, Unity project bridge
├── harness-ui/          # Ratatui TUI dashboard & Axum WebSocket web monitor
├── harness-cli/         # Unified executable (binary name: `dex`)
└── harness-desktop/     # Native macOS Desktop Application (`Dex.app`) via Tauri v2
```

---

## 🎙️ Audio, Voice & Deep Document OCR (`harness-audio`, `harness-docs`)

- **Native Microphone & Push-to-Talk:** Captures 16kHz mono PCM WAV directly in Rust (`cpal` + `hound`) with live RMS dB telemetry (`VoiceCaptureState`).
- **Pluggable STT & TTS:** Auto-detects HTTP Whisper (`/v1/audio/transcriptions`), `mlx_whisper`, or `whisper-cli` for transcription, and synthesizes speech via sidecars or automatic native macOS (`say` + `afconvert`) fallback, plus music & SFX generation via `ExtensionSupervisor`.
- **Apple Vision OCR & PDF Parser:** Compiles a native Swift Apple Vision helper on demand (`.dex/bin/dex-vision-ocr`, multilingual `es-ES`/`en-US`, Tesseract fallback) and extracts PDFs/DOCX/images into clean Markdown via Docling or pure-Rust `lopdf` fallback.

```bash
dex audio record --seconds 5
dex audio transcribe .dex/media/audio/recordings/rec.wav --timestamps
dex audio speak "Hola desde Dex-Harness" --voice Monica
dex docs ocr ./screenshot.png
dex docs parse ./report.pdf
```

---

## 🎨 Visual Studio: Image, Upscaling & Declarative Video Editing (`harness-media`)

- **Multi-Backend Image & Video Generation:** Automatically routes to local Apple Silicon MLX (`mlx-gen` / `mflux-generate`), ComfyUI workflows (`.dex/workflows/*.json` with safe placeholder injection), or OpenAI-compatible HTTP endpoints while broadcasting live `MediaGenerationProgress` and `MediaGenerated` events.
- **Pure-Rust Fast Image Ops & Lanczos3 Upscaling:** Non-blocking crop, resize, WebP/PNG/JPG conversion, and deterministic 2x–8x Lanczos3 upscaling fallback (`image` crate inside `spawn_blocking`).
- **Safe Declarative FFmpeg Video Editing:** Typed JSON `TimelineSpec` (clip trims, concat, external audio overlay, SRT subtitles, scaling, FPS, container selection) compiled directly to typed `ffmpeg` CLI arguments with zero shell interpolation, plus `ffprobe` metadata inspection and keyframe extraction.
- **Native Desktop Studio (`Dex.app`):** Dedicated **🎨 Media Studio** tab with generator form, ComfyUI workflow picker, live progress bar, reactive image/video gallery, 1-click **Upscale 2x**, **Enviar al Agente**, and **Finder** reveal (`open -R`).

```bash
dex media imagine "Isometric cyberpunk workshop in Tokyo, cinematic lighting" --width 1024 --height 1024 --steps 20 --backend auto
dex media upscale ./artwork.png --scale 2 --backend auto
dex media process ./artwork.png --resize 512x512 --format webp --out ./thumb.webp
dex media inspect-video ./clip.mp4
dex media extract-frames ./clip.mp4 --fps 1
dex media render-timeline ./timeline.json --out ./final.mp4
dex media list
```

---

## 🧊 3D & Game Studio (`harness-3d`)

- **Pure-Rust glTF 2.0 / OBJ Geometry Inspector:** Analyzes `.glb`, `.gltf`, and `.obj` files natively inside `spawn_blocking` (vertices, triangles, materials, normals, UVs, animations, bounding box dimensions, and real-time *game-ready* diagnostics) plus built-in binary `.glb` sample cube synthesis (`dex 3d sample`).
- **Image-to-3D Sidecar Orchestrator:** Lazily wakes `trellis-3d` via `ExtensionSupervisor` (or connects to Hunyuan3D / TripoSR / custom `DEX_3D_ENDPOINT`), saves `.glb` meshes into `.dex/media/3d/`, auto-inspects geometry, and streams `MediaGenerationProgress` & `MediaGenerated`.
- **Headless Blender Bridge:** Autodetects Blender (`BLENDER_BIN`, `$PATH`, `/Applications/Blender.app`), converts between `.glb`/`.gltf`/`.fbx`/`.obj`/`.blend` with optional LOD polygon decimation (`0.05..=1.0`), and renders auto-framed 3-point studio PNG previews via injection-safe generated Python scripts.
- **Direct Unity Project Tools:** Inspects Unity projects directly on disk (`ProjectVersion.txt`, `URP`/`HDRP`/`Built-in` pipeline detection, `.unity` scenes, C# script counts) and exports 3D assets + companion textures safely into `Assets/` with anti-path-traversal and overwrite guards.
- **100% Offline WebGL2 `.glb` Viewer in `Dex.app`:** Zero-CDN pure-JavaScript WebGL2 viewer inside the **🧊 3D & Game Studio** tab with binary glTF 2.0 parsing, orbit/zoom controls, Solid/Wireframe mode, reference grid, live HUD metrics, and 1-click Blender/Unity actions.

```bash
dex 3d sample
dex 3d inspect .dex/media/3d/sample_cube.glb
dex 3d generate --image .dex/media/images/concept.png --backend auto
dex 3d convert .dex/media/3d/sample_cube.glb --out ./hero_lod1.fbx --decimate 0.5
dex 3d preview .dex/media/3d/sample_cube.glb
dex 3d unity-inspect ~/UnityProjects/MyGame
dex 3d unity-export .dex/media/3d/sample_cube.glb --project ~/UnityProjects/MyGame --subdir Assets/DexGenerated
dex 3d list
```

---

## 🧩 Extensions Hub, Deferred Tools & Auto-Unload (`harness-extensions`)

Connect dozens of creative and engineering engines (Blender, Unity, Soundbox, ComfyUI, Docling, MLX-Gen, TRELLIS 3D, or custom MCP servers) without saturating local LLM context windows or wasting RAM:

- **Deferred Tool Discovery (`search_tools`):** External MCP tools register as `ToolVisibility::Deferred { category, tags }` and stay hidden until the LLM discovers and activates them on demand via `search_tools`.
- **Oversized Output Guardrail (`ToolOutputCompactor`):** Automatically spills bulky tool responses (> 8,000 chars) to `.dex/tmp/tool_outputs/<id>.txt`, returning a clean head/tail summary with file path for targeted `read_file` inspection.
- **Sidecar Supervisor with Idle Auto-Unload:** Lazily wakes extensions on demand, tracks `last_used_at`, and automatically puts idle processes to sleep after `idle_timeout_secs` (default 600s) to reclaim RAM for local LLMs.

```bash
# Browse the built-in curated catalog (Audio, Image/Video, 3D, OCR/Docs, GameDev):
dex ext catalog

# Install a catalog recipe into ~/.dex/extensions/extensions.json:
dex ext install blender-mcp

# Register any custom MCP server (stdio or SSE):
dex ext add-mcp --id my-fs --name "Filesystem MCP" --command npx --args "-y @modelcontextprotocol/server-filesystem /tmp"

# Manage lifecycle & view live status:
dex ext list
dex ext enable blender-mcp
dex ext start blender-mcp
dex ext stop blender-mcp
```

---

## 🧠 Local Model Hub (`harness-models`)

Dex-Harness features a native, LM Studio / Ollama-grade **Model Hub** directly in Rust:

- **Hugging Face Hub Search:** Search millions of open-source GGUF models (`https://huggingface.co/api/models`) with automatic extraction of available quantizations (`Q4_K_M`, `Q5_K_M`, `Q8_0`, `BF16`, etc.).
- **Hardware RAM Compatibility Calculation:** Evaluates model footprint against detected host RAM and Apple Silicon unified memory, grading each quantization with `🟢 Fits`, `🟡 Tight`, or `🔴 Too Large`.
- **Resumable Async Streaming Downloader:** Supports `HTTP Range` headers to resume interrupted downloads, writes to `.part` files, and renames atomically upon completion.
- **Ollama Bridge & Local Importer:** Pull models from Ollama library with streaming progress, import arbitrary `.gguf` files into `~/.dex/models/`, and automatically register imported models into local Ollama via `FROM /path/to/model.gguf`.

```bash
# Search GGUF models on Hugging Face:
dex models search "qwen2.5-coder"

# Pull a specific quantization from Hugging Face:
dex models pull Qwen/Qwen2.5-Coder-14B-Instruct-GGUF --quant Q4_K_M

# Pull directly from Ollama library:
dex models pull ollama:qwen2.5:7b

# Import any local .gguf file (and optionally register into Ollama):
dex models import ./my-model.gguf --name "custom-qwen" --to-ollama

# List all local Dex + Ollama models:
dex models list

# Remove a model from registry and disk:
dex models rm local:custom-qwen
```

---

## 🍏 Native macOS Desktop App (`Dex.app`)

**Dex-Harness** includes a first-class native macOS application built with **Tauri v2** and Apple WKWebView:

- **Global Shortcut:** Press `Option + Space` (or `Cmd + Shift + D`) from anywhere to instantly toggle the Dex floating window over your active apps (Raycast / Claude Desktop style).
- **System Tray (Menu Bar):** High-priority menu bar item with quick actions (*Open/Hide Dex*, *Autopilot Mode*, *Safe Mode*, *Plan Mode*, *Hardware Cookbook*, *Quit*).
- **Interactive Approval Modal:** In `--mode safe`, when Dex attempts sensitive actions (`execute_shell`, `write_file`, `desktop_mouse`, `git_worktree`), an interactive modal appears allowing one-click authorization (`Allow` or `Deny`).
- **Hardware Cookbook & Telemetry:** Native inspection of Apple Silicon Unified RAM, CPU cores, active subagents tree, and screenshot gallery.
- **macOS Permissions:** Preconfigured `Info.plist` and `entitlements.plist` requesting *Screen Recording* (`xcap`) and *Accessibility / Automation* (`enigo`).

### Building and Running `Dex.app`:

```bash
# Run in desktop development mode:
cargo run -p harness-desktop

# Or package the standalone macOS .app bundle:
cargo tauri build --bundles app --debug --no-sign
# Output: target/debug/bundle/macos/Dex.app
```

---

## 🛠️ Complete Native Tools Catalog

| Tool | Crate | Category | Description |
| :--- | :--- | :--- | :--- |
| `read_file` | `harness-tools` | Filesystem | Reads text files with line range slicing and line numbers. |
| `write_file` | `harness-tools` | Filesystem | Writes or overwrites files with automatic directory creation. |
| `edit_file` | `harness-tools` | Filesystem | Surgically replaces a unique text block and generates unified diffs. |
| `find_files` | `harness-tools` | Search | Rapidly discovers file paths respecting `.gitignore`. |
| `grep_search` | `harness-tools` | Search | Regex search across workspace files respecting `.gitignore`. |
| `execute_shell`| `harness-tools` | Terminal | Native asynchronous shell command execution with stdout/stderr pipes. |
| `pty_shell` | `harness-tools` | Terminal | Pseudo-terminal execution (`portable-pty`) with virtual terminal sizing. |
| `git_worktree` | `harness-tools` | Git | Manages isolated Git worktrees (`list`, `add`, `remove`) for parallel workflows. |
| `manage_tasks` | `harness-core` | Tasks | Tracks tasks (`pending`, `in_progress`, `completed`, `blocked`) on `.dex/tasks.json`. |
| `load_skill` | `harness-core` | Skills | Dynamically retrieves full instructions for skills in `.dex/skills/`. |
| `spawn_subagent`| `harness-core` | Orchestration| Concurrently spawns specialized subagents (`Explorer`, `Coder`, `Reviewer`). |
| `browser_navigate`| `harness-computer`| Browser | Navigates to a URL via Chrome DevTools Protocol (headless or visible). |
| `browser_extract_dom`| `harness-computer`| Browser | Extracts clean text/DOM without scripts or styles for token efficiency. |
| `browser_click`| `harness-computer`| Browser | Clicks web elements using CSS selectors. |
| `browser_type`| `harness-computer`| Browser | Enters text into input fields using CSS selectors. |
| `browser_screenshot`| `harness-computer`| Browser | Takes webpage screenshot and saves to `.dex/screenshots/`. |
| `desktop_screenshot`| `harness-computer`| Desktop | Captures primary desktop monitor and saves to `.dex/screenshots/`. |
| `desktop_mouse`| `harness-computer`| Desktop | Simulates mouse movement, clicks, and drags with coordinate boundary checks. |
| `desktop_keyboard`| `harness-computer`| Desktop | Simulates keyboard typing and special keys (Return, Escape, Tab, etc.). |
| `image_generate` | `harness-media` | ImageVideo (Deferred) | Generates images via MLX (`mlx-gen`/`mflux`), ComfyUI workflows, or HTTP endpoints. |
| `image_upscale` | `harness-media` | ImageVideo (Deferred) | Upscales images 2x–8x via neural sidecars or deterministic pure-Rust Lanczos3 fallback. |
| `image_process` | `harness-media` | ImageVideo (Deferred) | Fast native pure-Rust crop, resize, and PNG/JPG/WebP conversion. |
| `video_generate` | `harness-media` | ImageVideo (Deferred) | Generates short text-to-video / image-to-video clips via MLX or ComfyUI. |
| `video_inspect` | `harness-media` | ImageVideo (Deferred) | Inspects video duration, resolution, FPS, codecs, and bitrate via `ffprobe`. |
| `video_extract_frames` | `harness-media` | ImageVideo (Deferred) | Extracts keyframes as PNGs into `.dex/media/frames/` for visual inspection or OCR. |
| `video_edit` | `harness-media` | ImageVideo (Deferred) | Renders declarative JSON `TimelineSpec` edits safely via `ffmpeg`. |
| `mesh_inspect` | `harness-3d` | ThreeD (Deferred) | Pure-Rust `.glb`/`.gltf`/`.obj` inspector with bounding box and *game-ready* warnings. |
| `mesh_generate_3d` | `harness-3d` | ThreeD (Deferred) | Generates `.glb` meshes from images/prompts via TRELLIS 2 / Hunyuan3D / TripoSR / HTTP sidecars. |
| `blender_convert_mesh` | `harness-3d` | ThreeD (Deferred) | Headless Blender format converter (`.glb`/`.fbx`/`.obj`/`.blend`) with LOD decimation. |
| `blender_render_preview` | `harness-3d` | ThreeD (Deferred) | Renders an auto-framed studio preview PNG of a 3D mesh in headless Blender. |
| `unity_inspect_project` | `harness-3d` | GameDev (Deferred) | Detects Unity version, render pipeline (URP/HDRP/Built-in), `.unity` scenes, and C# scripts. |
| `unity_export_asset` | `harness-3d` | GameDev (Deferred) | Safely copies a 3D model and companion textures into a Unity project's `Assets/` folder. |

---

## 💻 CLI Usage (`dex`)

```bash
# 1. Run an autonomous task in Autopilot mode
dex run "Inspect crates/ and run tests"

# 2. Run in Safe mode (requires human approval for file writes, shell, or desktop actions)
dex run "Refactor codebase" --mode safe

# 3. Run in Plan mode (strictly read-only)
dex run "Design subagent architecture" --mode plan

# 4. Launch interactive REPL mode
dex repl

# 5. Start as an MCP Server for Claude Desktop or Cursor
dex mcp-serve

# 6. Launch the live Terminal UI (TUI) Dashboard
dex tui

# 7. Start the embedded Web Dashboard on http://localhost:3000
dex web --port 3000

# 8. Check hardware profiling and local model recommendations
dex cookbook

# 9. Manage scheduled tasks and run daemon
dex schedule add --name "Lint Task" --goal "Run cargo clippy" --interval 3600
dex schedule list
dex daemon
```

---

## 🛡️ Engineering Standards

Dex-Harness strictly follows the invariants established in [AGENTS.md](file:///Users/robii/Documents/GitHub/Dex-Harness/AGENTS.md):
- **Zero Panics:** No `.unwrap()` or `.expect()` in production crates.
- **Type Safety:** All tools derive `schemars::JsonSchema`.
- **Async Concurrency:** Non-blocking Tokio I/O with timeouts and structured task joins.

---

## 📄 License

Dual-licensed under either Apache License, Version 2.0 or MIT License at your option.
