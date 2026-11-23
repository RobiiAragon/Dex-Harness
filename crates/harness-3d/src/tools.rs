use std::path::Path;
use std::sync::Arc;
use async_trait::async_trait;
use harness_core::{EventBus, Tool, ToolError, ToolRegistry};
use harness_extensions::ExtensionSupervisor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::blender::{blender_convert_mesh, blender_render_preview};
use crate::gen3d::{generate_3d_mesh, Gen3dBackend, Gen3dRequest};
use crate::mesh_ops::inspect_mesh;
use crate::unity::{unity_export_asset, unity_inspect_project};

/// Arguments for `mesh_inspect`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MeshInspectArgs {
    /// Path to a 3D mesh file (.glb, .gltf, or .obj).
    pub path: String,
}

/// Inspects a 3D mesh natively in pure Rust (vertices, triangles, materials, bounding box, game-ready checks).
pub struct MeshInspectTool;

#[async_trait]
impl Tool for MeshInspectTool {
    fn name(&self) -> &str {
        "mesh_inspect"
    }

    fn description(&self) -> &str {
        "Inspects a 3D model (.glb, .gltf, or .obj) natively in pure Rust, returning vertices, triangles, materials, normals/UVs, animations, bounding box dimensions, and game-ready warnings as JSON."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(MeshInspectArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: MeshInspectArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let report = inspect_mesh(Path::new(&args.path))
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: e.to_string(),
            })?;

        serde_json::to_string_pretty(&report).map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: format!("Failed to serialize mesh report: {e}"),
        })
    }
}

fn default_texture_res() -> u32 {
    1024
}

fn default_remove_bg() -> bool {
    true
}

/// Arguments for `mesh_generate_3d`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MeshGenerate3dArgs {
    /// Path to an input reference image (e.g. from `.dex/media/images/`).
    #[serde(default)]
    pub image_path: Option<String>,
    /// Optional text prompt describing the 3D object.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Backend selector: `auto`, `trellis`, `hunyuan3d`, `tripo_sr`, or `http`.
    #[serde(default)]
    pub backend: Gen3dBackend,
    /// PBR texture resolution in pixels (256..=4096, default: 1024).
    #[serde(default = "default_texture_res")]
    pub texture_resolution: u32,
    /// Whether to strip background before 3D reconstruction (default: true).
    #[serde(default = "default_remove_bg")]
    pub remove_background: bool,
}

/// Generates a 3D `.glb` mesh via local 3D sidecars (`trellis-3d`, Hunyuan3D, TripoSR, or HTTP).
pub struct MeshGenerate3dTool {
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
}

impl MeshGenerate3dTool {
    pub fn new(supervisor: Option<Arc<ExtensionSupervisor>>, event_bus: Option<EventBus>) -> Self {
        Self {
            supervisor,
            event_bus,
        }
    }
}

#[async_trait]
impl Tool for MeshGenerate3dTool {
    fn name(&self) -> &str {
        "mesh_generate_3d"
    }

    fn description(&self) -> &str {
        "Generates a 3D `.glb` mesh from a reference image and/or prompt using local 3D sidecars (TRELLIS 2 / Hunyuan3D / TripoSR / HTTP), saves to `.dex/media/3d/`, and automatically inspects the resulting geometry."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(MeshGenerate3dArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: MeshGenerate3dArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let req = Gen3dRequest {
            image_path: args.image_path,
            prompt: args.prompt,
            backend: args.backend,
            texture_resolution: args.texture_resolution,
            remove_background: args.remove_background,
        };

        let res = generate_3d_mesh(req, self.supervisor.clone(), self.event_bus.as_ref())
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: e.to_string(),
            })?;

        Ok(format!(
            "3D mesh generated via {} ({} vertices, {} triangles, {} bytes). Saved to: {}",
            res.backend,
            res.report.vertex_count,
            res.report.triangle_count,
            res.report.file_size_bytes,
            res.path.display()
        ))
    }
}

fn default_apply_transforms() -> bool {
    true
}

/// Arguments for `blender_convert_mesh`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlenderConvertMeshArgs {
    /// Input 3D model path (.glb, .gltf, .fbx, .obj, .blend).
    pub input_path: String,
    /// Target output 3D model path (.glb, .gltf, .fbx, .obj, .blend).
    pub output_path: String,
    /// Optional polygon decimation ratio in `0.05..=1.0` for generating LODs.
    #[serde(default)]
    pub decimate_ratio: Option<f32>,
    /// Whether to apply location/rotation/scale transforms before export (default: true).
    #[serde(default = "default_apply_transforms")]
    pub apply_transforms: bool,
}

/// Converts and optionally decimates a 3D mesh using headless Blender.
pub struct BlenderConvertMeshTool {
    event_bus: Option<EventBus>,
}

impl BlenderConvertMeshTool {
    pub fn new(event_bus: Option<EventBus>) -> Self {
        Self { event_bus }
    }
}

#[async_trait]
impl Tool for BlenderConvertMeshTool {
    fn name(&self) -> &str {
        "blender_convert_mesh"
    }

    fn description(&self) -> &str {
        "Runs Blender headlessly to convert a 3D model between `.glb`, `.gltf`, `.fbx`, `.obj`, and `.blend`, with optional polygon decimation (`0.05..=1.0`) for game LODs and transform application."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(BlenderConvertMeshArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: BlenderConvertMeshArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let res = blender_convert_mesh(
            Path::new(&args.input_path),
            Path::new(&args.output_path),
            args.decimate_ratio,
            args.apply_transforms,
            self.event_bus.as_ref(),
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        let tri_note = res
            .mesh_report
            .as_ref()
            .map(|r| format!(", {} triangles", r.triangle_count))
            .unwrap_or_default();
        Ok(format!(
            "Converted '{}' -> '{}' ({} bytes{tri_note}).",
            res.input_path.display(),
            res.output_path.display(),
            res.file_size_bytes
        ))
    }
}

fn default_preview_dim() -> u32 {
    768
}

/// Arguments for `blender_render_preview`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlenderRenderPreviewArgs {
    /// Input 3D model path (.glb, .gltf, .fbx, .obj, .blend).
    pub input_path: String,
    /// Optional destination PNG path (defaults to `.dex/media/3d/previews/`).
    #[serde(default)]
    pub output_png: Option<String>,
    /// Output preview width in pixels (default: 768).
    #[serde(default = "default_preview_dim")]
    pub width: u32,
    /// Output preview height in pixels (default: 768).
    #[serde(default = "default_preview_dim")]
    pub height: u32,
}

/// Renders an auto-framed studio preview PNG of a 3D mesh using headless Blender.
pub struct BlenderRenderPreviewTool {
    event_bus: Option<EventBus>,
}

impl BlenderRenderPreviewTool {
    pub fn new(event_bus: Option<EventBus>) -> Self {
        Self { event_bus }
    }
}

#[async_trait]
impl Tool for BlenderRenderPreviewTool {
    fn name(&self) -> &str {
        "blender_render_preview"
    }

    fn description(&self) -> &str {
        "Imports a 3D model into headless Blender, auto-frames a studio camera and 3-point lighting around its bounding box, and renders a PNG preview into `.dex/media/3d/previews/`."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(BlenderRenderPreviewArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: BlenderRenderPreviewArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let res = blender_render_preview(
            Path::new(&args.input_path),
            args.output_png.as_deref().map(Path::new),
            args.width,
            args.height,
            self.event_bus.as_ref(),
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        Ok(format!(
            "Rendered {}x{} studio preview ({} bytes) to: {}",
            res.width,
            res.height,
            res.file_size_bytes,
            res.preview_png.display()
        ))
    }
}

/// Arguments for `unity_inspect_project`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UnityInspectProjectArgs {
    /// Path to the root directory of a Unity project (containing `Assets/`).
    pub project_dir: String,
}

/// Inspects a Unity project directly on disk (Unity version, URP/HDRP/Built-in pipeline, scenes, C# scripts).
pub struct UnityInspectProjectTool;

#[async_trait]
impl Tool for UnityInspectProjectTool {
    fn name(&self) -> &str {
        "unity_inspect_project"
    }

    fn description(&self) -> &str {
        "Inspects a Unity project directory on disk without requiring the Unity Editor: detects Unity version, render pipeline (URP / HDRP / Built-in), `.unity` scenes, and C# script count."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(UnityInspectProjectArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: UnityInspectProjectArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let report = unity_inspect_project(Path::new(&args.project_dir))
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: e.to_string(),
            })?;

        serde_json::to_string_pretty(&report).map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: format!("Failed to serialize Unity project report: {e}"),
        })
    }
}

/// Arguments for `unity_export_asset`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UnityExportAssetArgs {
    /// Path to the 3D model file (.glb, .gltf, .fbx, .obj) to copy into Unity.
    pub mesh_path: String,
    /// Root path of the Unity project (must contain `Assets/`).
    pub project_dir: String,
    /// Relative destination subdirectory inside `Assets/` (default: `Assets/DexGenerated`).
    #[serde(default)]
    pub subdir: Option<String>,
    /// Whether to overwrite an existing asset at the destination (default: false).
    #[serde(default)]
    pub overwrite: bool,
}

/// Safely exports a 3D model and companion textures/materials into a Unity project's `Assets/` folder.
pub struct UnityExportAssetTool;

#[async_trait]
impl Tool for UnityExportAssetTool {
    fn name(&self) -> &str {
        "unity_export_asset"
    }

    fn description(&self) -> &str {
        "Copies a 3D model (.glb, .gltf, .fbx, .obj) and matching companion textures/materials into a Unity project's `Assets/` folder with strict path-traversal and overwrite guards."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(UnityExportAssetArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: UnityExportAssetArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let res = unity_export_asset(
            Path::new(&args.mesh_path),
            Path::new(&args.project_dir),
            args.subdir.as_deref(),
            args.overwrite,
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        Ok(format!(
            "Exported {} file(s) to Unity folder '{}'. Primary model: {}",
            res.copied_files.len(),
            res.destination_dir.display(),
            res.exported_model_path.display()
        ))
    }
}

/// Registers all 6 3D & GameDev tools as `ToolVisibility::Deferred` under `"ThreeD"` and `"GameDev"`.
pub fn register_3d_tools(
    registry: &mut ToolRegistry,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
) {
    // Category: ThreeD (4 tools)
    registry.register_deferred(
        Arc::new(MeshInspectTool),
        "ThreeD",
        vec![
            "3d".to_string(),
            "mesh".to_string(),
            "glb".to_string(),
            "gltf".to_string(),
            "obj".to_string(),
            "inspect".to_string(),
            "triangles".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(MeshGenerate3dTool::new(supervisor, event_bus.clone())),
        "ThreeD",
        vec![
            "3d".to_string(),
            "mesh".to_string(),
            "generate".to_string(),
            "trellis".to_string(),
            "hunyuan3d".to_string(),
            "triposr".to_string(),
            "glb".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(BlenderConvertMeshTool::new(event_bus.clone())),
        "ThreeD",
        vec![
            "3d".to_string(),
            "blender".to_string(),
            "convert".to_string(),
            "decimate".to_string(),
            "fbx".to_string(),
            "lod".to_string(),
            "glb".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(BlenderRenderPreviewTool::new(event_bus)),
        "ThreeD",
        vec![
            "3d".to_string(),
            "blender".to_string(),
            "render".to_string(),
            "preview".to_string(),
            "thumbnail".to_string(),
        ],
    );

    // Category: GameDev (2 tools)
    registry.register_deferred(
        Arc::new(UnityInspectProjectTool),
        "GameDev",
        vec![
            "unity".to_string(),
            "gamedev".to_string(),
            "project".to_string(),
            "urp".to_string(),
            "hdrp".to_string(),
            "scenes".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(UnityExportAssetTool),
        "GameDev",
        vec![
            "unity".to_string(),
            "gamedev".to_string(),
            "export".to_string(),
            "asset".to_string(),
            "3d".to_string(),
            "glb".to_string(),
            "fbx".to_string(),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_3d_and_gamedev_tools_registered_as_deferred() {
        let mut registry = ToolRegistry::new();
        register_3d_tools(&mut registry, None, None);

        assert!(registry.active_definitions().is_empty());
        assert_eq!(registry.deferred_count(), 6);

        let three_d = registry.search_deferred("", Some("ThreeD"));
        assert_eq!(three_d.len(), 4);

        let game_dev = registry.search_deferred("", Some("GameDev"));
        assert_eq!(game_dev.len(), 2);

        let decimate_hits = registry.search_deferred("decimate", Some("ThreeD"));
        assert_eq!(decimate_hits.len(), 1);
        assert_eq!(decimate_hits[0].name, "blender_convert_mesh");

        let unity_hits = registry.search_deferred("urp", Some("GameDev"));
        assert_eq!(unity_hits.len(), 1);
        assert_eq!(unity_hits[0].name, "unity_inspect_project");
    }
}
