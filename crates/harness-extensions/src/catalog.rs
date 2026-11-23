use std::collections::HashMap;
use crate::manifest::{
    ExtensionCategory, ExtensionKind, ExtensionManifest, DEFAULT_IDLE_TIMEOUT_SECS,
};

/// Returns the built-in curated catalog of MCP servers and local creative/engineering sidecars.
pub fn builtin_catalog() -> Vec<ExtensionManifest> {
    vec![
        ExtensionManifest {
            id: "soundbox-mcp".to_string(),
            name: "Soundbox MCP".to_string(),
            category: ExtensionCategory::Audio,
            description: "Generación de música, efectos de sonido (SFX), síntesis de voz (TTS) y clonación de voz.".to_string(),
            kind: ExtensionKind::McpStdio {
                command: "uvx".to_string(),
                args: vec!["soundbox-mcp".to_string()],
                env: HashMap::new(),
            },
            estimated_ram_mb: 2560,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            install_hint: "Requiere `uv` (brew install uv). Ejecuta `uvx soundbox-mcp` o configura tu clave/modelo local.".to_string(),
            enabled: false,
            tags: vec![
                "audio".to_string(),
                "music".to_string(),
                "sfx".to_string(),
                "tts".to_string(),
                "voice".to_string(),
            ],
        },
        ExtensionManifest {
            id: "docling-mcp".to_string(),
            name: "Docling MCP".to_string(),
            category: ExtensionCategory::OcrDocs,
            description: "OCR de alta precisión y extracción estructurada de PDFs, tablas, DOCX y presentaciones.".to_string(),
            kind: ExtensionKind::McpStdio {
                command: "uvx".to_string(),
                args: vec!["--from".to_string(), "docling-mcp".to_string(), "docling-mcp-server".to_string()],
                env: HashMap::new(),
            },
            estimated_ram_mb: 1800,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            install_hint: "Requiere `uv` (brew install uv). Se descarga automáticamente con `uvx --from docling-mcp docling-mcp-server`.".to_string(),
            enabled: false,
            tags: vec![
                "ocr".to_string(),
                "pdf".to_string(),
                "documents".to_string(),
                "markdown".to_string(),
                "tables".to_string(),
            ],
        },
        ExtensionManifest {
            id: "mlx-gen".to_string(),
            name: "MLX Gen (Apple Silicon)".to_string(),
            category: ExtensionCategory::ImageVideo,
            description: "Sidecar nativo de generación de imágenes y video acelerado en GPU Apple Silicon con MLX (FLUX / SDXL).".to_string(),
            kind: ExtensionKind::LocalSidecar {
                command: "python3".to_string(),
                args: vec!["-m".to_string(), "mflux.server".to_string(), "--port".to_string(), "8190".to_string()],
                port: 8190,
                healthcheck_url: "http://127.0.0.1:8190/health".to_string(),
                env: HashMap::new(),
            },
            estimated_ram_mb: 6144,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            install_hint: "Instala mflux en Python 3.11+: `uv tool install mflux` o `pip install mflux`.".to_string(),
            enabled: false,
            tags: vec![
                "mlx".to_string(),
                "image".to_string(),
                "video".to_string(),
                "flux".to_string(),
                "apple-silicon".to_string(),
            ],
        },
        ExtensionManifest {
            id: "comfyui-mcp".to_string(),
            name: "ComfyUI MCP".to_string(),
            category: ExtensionCategory::ImageVideo,
            description: "Orquestación de pipelines visuales en ComfyUI: LoRAs, ControlNet, inpainting y generación de texturas.".to_string(),
            kind: ExtensionKind::McpStdio {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "comfyui-mcp-server".to_string()],
                env: HashMap::from([("COMFYUI_URL".to_string(), "http://127.0.0.1:8188".to_string())]),
            },
            estimated_ram_mb: 8192,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            install_hint: "Requiere Node.js (`npx`) y una instancia local de ComfyUI escuchando en http://127.0.0.1:8188.".to_string(),
            enabled: false,
            tags: vec![
                "comfyui".to_string(),
                "image".to_string(),
                "lora".to_string(),
                "controlnet".to_string(),
                "textures".to_string(),
            ],
        },
        ExtensionManifest {
            id: "blender-mcp".to_string(),
            name: "Blender MCP".to_string(),
            category: ExtensionCategory::ThreeD,
            description: "Control directo de escenas 3D, modelado procedural, materiales y renderizado en Blender.".to_string(),
            kind: ExtensionKind::McpStdio {
                command: "uvx".to_string(),
                args: vec!["blender-mcp".to_string()],
                env: HashMap::new(),
            },
            estimated_ram_mb: 3072,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            install_hint: "Instala el add-on BlenderMCP en Blender y habilita el socket MCP antes de conectar (`uvx blender-mcp`).".to_string(),
            enabled: false,
            tags: vec![
                "blender".to_string(),
                "3d".to_string(),
                "render".to_string(),
                "mesh".to_string(),
                "materials".to_string(),
            ],
        },
        ExtensionManifest {
            id: "unity-mcp".to_string(),
            name: "Unity MCP".to_string(),
            category: ExtensionCategory::GameDev,
            description: "Inspección y edición de escenas de Unity, creación de scripts C#, prefabs y lectura de la consola del editor.".to_string(),
            kind: ExtensionKind::McpStdio {
                command: "uvx".to_string(),
                args: vec!["unity-mcp".to_string()],
                env: HashMap::new(),
            },
            estimated_ram_mb: 2048,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            install_hint: "Añade el paquete Unity MCP Bridge en Unity Package Manager y ejecuta `uvx unity-mcp`.".to_string(),
            enabled: false,
            tags: vec![
                "unity".to_string(),
                "gamedev".to_string(),
                "csharp".to_string(),
                "scene".to_string(),
                "console".to_string(),
            ],
        },
        ExtensionManifest {
            id: "trellis-3d".to_string(),
            name: "TRELLIS 3D Sidecar".to_string(),
            category: ExtensionCategory::ThreeD,
            description: "Sidecar local de reconstrucción imagen-a-3D que exporta mallas `.glb` con materiales PBR.".to_string(),
            kind: ExtensionKind::LocalSidecar {
                command: "python3".to_string(),
                args: vec!["-m".to_string(), "trellis_server".to_string(), "--port".to_string(), "8195".to_string()],
                port: 8195,
                healthcheck_url: "http://127.0.0.1:8195/health".to_string(),
                env: HashMap::new(),
            },
            estimated_ram_mb: 10240,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            install_hint: "En Apple Silicon usa `vinayapathak/trellis-mac-mps` (`python3 cli.py serve`) o `dgrauet/Hunyuan3D-2.1-mlx` en el puerto 8195.".to_string(),
            enabled: false,
            tags: vec![
                "trellis".to_string(),
                "3d".to_string(),
                "glb".to_string(),
                "pbr".to_string(),
                "image-to-3d".to_string(),
            ],
        },
        ExtensionManifest {
            id: "fish-audio-s2-pro".to_string(),
            name: "Fish Audio S2-Pro Sidecar".to_string(),
            category: ExtensionCategory::Audio,
            description: "Motor TTS expresivo y clonación zero-shot multilingüe de Fish Speech S2-Pro sobre endpoint HTTP local.".to_string(),
            kind: ExtensionKind::LocalSidecar {
                command: "python3".to_string(),
                args: vec!["-m".to_string(), "tools.api_server".to_string(), "--listen".to_string(), "127.0.0.1:8080".to_string()],
                port: 8080,
                healthcheck_url: "http://127.0.0.1:8080/v1/health".to_string(),
                env: HashMap::new(),
            },
            estimated_ram_mb: 6144,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            install_hint: "Clona `fishaudio/fish-speech` y arranca `python -m tools.api_server --listen 127.0.0.1:8080` (compatible con Voice Studio).".to_string(),
            enabled: false,
            tags: vec![
                "audio".to_string(),
                "tts".to_string(),
                "voice-cloning".to_string(),
                "fish-speech".to_string(),
            ],
        },
        ExtensionManifest {
            id: "step-audio-editx".to_string(),
            name: "Step-Audio EditX Sidecar".to_string(),
            category: ExtensionCategory::Audio,
            description: "Modelo multimodal de síntesis, edición expresiva y clonación de voz Step-Audio EditX.".to_string(),
            kind: ExtensionKind::LocalSidecar {
                command: "python3".to_string(),
                args: vec!["-m".to_string(), "step_audio.server".to_string(), "--port".to_string(), "8192".to_string()],
                port: 8192,
                healthcheck_url: "http://127.0.0.1:8192/health".to_string(),
                env: HashMap::new(),
            },
            estimated_ram_mb: 8192,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            install_hint: "Configura `stepfun-ai/Step-Audio` en el puerto 8192 o define `DEX_TTS_URL` hacia tu servidor Step-Audio.".to_string(),
            enabled: false,
            tags: vec![
                "audio".to_string(),
                "tts".to_string(),
                "step-audio".to_string(),
                "voice-cloning".to_string(),
            ],
        },
        ExtensionManifest {
            id: "omnivoice-tts".to_string(),
            name: "OmniVoice (k2-fsa) Sidecar".to_string(),
            category: ExtensionCategory::Audio,
            description: "TTS y clonación zero-shot de 600+ idiomas basado en difusión + Qwen (motor por defecto de VoiceStudio).".to_string(),
            kind: ExtensionKind::LocalSidecar {
                command: "python3".to_string(),
                args: vec!["-m".to_string(), "omnivoice.server".to_string(), "--port".to_string(), "8193".to_string()],
                port: 8193,
                healthcheck_url: "http://127.0.0.1:8193/health".to_string(),
                env: HashMap::new(),
            },
            estimated_ram_mb: 4096,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            install_hint: "Instala `k2-fsa/OmniVoice` o ejecuta su servidor OpenAI-compatible en `http://127.0.0.1:8193`.".to_string(),
            enabled: false,
            tags: vec![
                "audio".to_string(),
                "tts".to_string(),
                "omnivoice".to_string(),
                "voice-cloning".to_string(),
            ],
        },
    ]
}

/// Finds a built-in catalog recipe by ID.
pub fn find_catalog_recipe(id: &str) -> Option<ExtensionManifest> {
    builtin_catalog()
        .into_iter()
        .find(|item| item.id.eq_ignore_ascii_case(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_builtin_catalog_completeness_and_uniqueness() {
        let catalog = builtin_catalog();
        assert_eq!(catalog.len(), 10);

        let expected_ids = [
            "soundbox-mcp",
            "docling-mcp",
            "mlx-gen",
            "comfyui-mcp",
            "blender-mcp",
            "unity-mcp",
            "trellis-3d",
            "fish-audio-s2-pro",
            "step-audio-editx",
            "omnivoice-tts",
        ];

        let mut seen = HashSet::new();
        for item in &catalog {
            assert!(seen.insert(item.id.clone()), "Duplicate catalog ID: {}", item.id);
            assert!(item.estimated_ram_mb > 0);
            assert_eq!(item.idle_timeout_secs, DEFAULT_IDLE_TIMEOUT_SECS);
            assert!(!item.install_hint.is_empty());
        }

        for id in expected_ids {
            assert!(find_catalog_recipe(id).is_some(), "Missing expected recipe: {id}");
        }
    }
}
