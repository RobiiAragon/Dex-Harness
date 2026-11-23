use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use harness_core::{DexEvent, EventBus};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, info};

use crate::error::ThreeDError;
use crate::mesh_ops::{inspect_mesh, MeshReport, DEFAULT_3D_PREVIEWS_DIR};

const BLENDER_TIMEOUT_SECS: u64 = 300;
const BLENDER_SCRIPTS_TMP_DIR: &str = ".dex/tmp/blender_scripts";

/// Availability and version summary of a local Blender installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlenderStatus {
    pub available: bool,
    pub executable: Option<String>,
    pub version: Option<String>,
}

/// Outcome of a headless Blender format conversion / decimation pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlenderConvertResult {
    pub input_path: PathBuf,
    pub output_path: PathBuf,
    pub decimate_ratio: Option<f32>,
    pub applied_transforms: bool,
    pub file_size_bytes: u64,
    pub mesh_report: Option<MeshReport>,
}

/// Outcome of a headless Blender studio preview render.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlenderPreviewResult {
    pub input_path: PathBuf,
    pub preview_png: PathBuf,
    pub width: u32,
    pub height: u32,
    pub file_size_bytes: u64,
}

fn current_epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Detects the Blender binary via `BLENDER_BIN`, `$PATH`, or standard macOS `.app` bundle paths.
pub async fn detect_blender_executable() -> Option<PathBuf> {
    if let Ok(env_bin) = std::env::var("BLENDER_BIN") {
        let p = PathBuf::from(env_bin.trim());
        if !p.as_os_str().is_empty() && tokio::fs::try_exists(&p).await.unwrap_or(false) {
            return Some(p);
        }
    }

    if let Some(found) = harness_core::find_executable_in_enriched_path("blender")
        .or_else(|| harness_core::find_executable_in_enriched_path("Blender"))
    {
        return Some(found);
    }

    #[cfg(target_os = "macos")]
    {
        let mut candidates = vec![PathBuf::from(
            "/Applications/Blender.app/Contents/MacOS/Blender",
        )];
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(
                PathBuf::from(home).join("Applications/Blender.app/Contents/MacOS/Blender"),
            );
        }
        for candidate in candidates {
            if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
                return Some(candidate);
            }
        }
    }

    None
}

/// Probes local Blender availability and parses its reported version string.
pub async fn check_blender_status() -> BlenderStatus {
    let Some(exe) = detect_blender_executable().await else {
        return BlenderStatus {
            available: false,
            executable: None,
            version: None,
        };
    };

    let version = match tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(&exe).arg("--version").output(),
    )
    .await
    {
        Ok(Ok(out)) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty()),
        _ => None,
    };

    BlenderStatus {
        available: true,
        executable: Some(exe.to_string_lossy().to_string()),
        version,
    }
}

/// Builds safe, strictly typed CLI arguments for invoking Blender headlessly.
pub fn build_blender_cli_args(script_path: &Path) -> Vec<String> {
    vec![
        "--background".to_string(),
        "--factory-startup".to_string(),
        "--python-exit-code".to_string(),
        "1".to_string(),
        "--python".to_string(),
        script_path.to_string_lossy().to_string(),
    ]
}

fn validate_mesh_ext(path: &Path, role: &str) -> Result<String, ThreeDError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !matches!(ext.as_str(), "glb" | "gltf" | "fbx" | "obj" | "blend") {
        return Err(ThreeDError::InvalidInput {
            reason: format!(
                "Unsupported {role} 3D format '.{ext}' at '{}'. Supported: .glb, .gltf, .fbx, .obj, .blend",
                path.display()
            ),
        });
    }
    Ok(ext)
}

fn json_literal_path(path: &Path, context: &str) -> Result<String, ThreeDError> {
    let raw = path.to_string_lossy().to_string();
    if raw.contains('\0') {
        return Err(ThreeDError::SecurityViolation {
            reason: format!("Null byte found in path ({context})"),
        });
    }
    serde_json::to_string(&raw).map_err(|e| ThreeDError::Json {
        context: context.to_string(),
        source: e,
    })
}

/// Generates a controlled, injection-safe Blender Python script to convert, optionally transform-apply,
/// and optionally decimate a 3D model between `.glb`, `.gltf`, `.fbx`, `.obj`, and `.blend`.
pub fn build_convert_script(
    input_path: &Path,
    output_path: &Path,
    decimate_ratio: Option<f32>,
    apply_transforms: bool,
) -> Result<String, ThreeDError> {
    let in_ext = validate_mesh_ext(input_path, "input")?;
    let out_ext = validate_mesh_ext(output_path, "output")?;

    if let Some(ratio) = decimate_ratio {
        if !ratio.is_finite() || !(0.05..=1.0).contains(&ratio) {
            return Err(ThreeDError::InvalidInput {
                reason: format!(
                    "decimate_ratio must be finite and in 0.05..=1.0 (got {ratio})"
                ),
            });
        }
    }

    let in_lit = json_literal_path(input_path, "blender input_path")?;
    let out_lit = json_literal_path(output_path, "blender output_path")?;
    let decimate_py = match decimate_ratio {
        Some(r) => format!("{r:.4}"),
        None => "None".to_string(),
    };
    let apply_py = if apply_transforms { "True" } else { "False" };

    Ok(format!(
        r#"import bpy
import os

INPUT_PATH = {in_lit}
OUTPUT_PATH = {out_lit}
IN_EXT = "{in_ext}"
OUT_EXT = "{out_ext}"
DECIMATE_RATIO = {decimate_py}
APPLY_TRANSFORMS = {apply_py}

bpy.ops.wm.read_factory_settings(use_empty=True)

if IN_EXT in ("glb", "gltf"):
    bpy.ops.import_scene.gltf(filepath=INPUT_PATH)
elif IN_EXT == "fbx":
    bpy.ops.import_scene.fbx(filepath=INPUT_PATH)
elif IN_EXT == "obj":
    if hasattr(bpy.ops.wm, "obj_import"):
        bpy.ops.wm.obj_import(filepath=INPUT_PATH)
    else:
        bpy.ops.import_scene.obj(filepath=INPUT_PATH)
elif IN_EXT == "blend":
    bpy.ops.wm.open_mainfile(filepath=INPUT_PATH)

meshes = [obj for obj in bpy.context.scene.objects if obj.type == "MESH"]

if APPLY_TRANSFORMS and meshes:
    bpy.ops.object.select_all(action="DESELECT")
    for obj in meshes:
        obj.select_set(True)
    bpy.context.view_layer.objects.active = meshes[0]
    bpy.ops.object.transform_apply(location=True, rotation=True, scale=True)

if DECIMATE_RATIO is not None and DECIMATE_RATIO < 1.0:
    for obj in meshes:
        bpy.ops.object.select_all(action="DESELECT")
        obj.select_set(True)
        bpy.context.view_layer.objects.active = obj
        mod = obj.modifiers.new(name="DexDecimate", type="DECIMATE")
        mod.ratio = float(DECIMATE_RATIO)
        bpy.ops.object.modifier_apply(modifier=mod.name)

os.makedirs(os.path.dirname(os.path.abspath(OUTPUT_PATH)), exist_ok=True)

if OUT_EXT == "glb":
    bpy.ops.export_scene.gltf(filepath=OUTPUT_PATH, export_format="GLB")
elif OUT_EXT == "gltf":
    bpy.ops.export_scene.gltf(filepath=OUTPUT_PATH, export_format="GLTF_SEPARATE")
elif OUT_EXT == "fbx":
    bpy.ops.export_scene.fbx(filepath=OUTPUT_PATH, use_selection=False)
elif OUT_EXT == "obj":
    if hasattr(bpy.ops.wm, "obj_export"):
        bpy.ops.wm.obj_export(filepath=OUTPUT_PATH)
    else:
        bpy.ops.export_scene.obj(filepath=OUTPUT_PATH)
elif OUT_EXT == "blend":
    bpy.ops.wm.save_as_mainfile(filepath=OUTPUT_PATH)
"#
    ))
}

/// Generates a controlled, injection-safe Blender Python script that imports a 3D model,
/// auto-frames a camera around its world bounding box with studio lighting, and renders a PNG preview.
pub fn build_render_preview_script(
    input_path: &Path,
    output_png: &Path,
    width: u32,
    height: u32,
) -> Result<String, ThreeDError> {
    let in_ext = validate_mesh_ext(input_path, "input")?;
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(ThreeDError::InvalidInput {
            reason: format!("Preview dimensions must be in 1..=4096 (got {width}x{height})"),
        });
    }

    let in_lit = json_literal_path(input_path, "blender preview input_path")?;
    let out_lit = json_literal_path(output_png, "blender preview output_png")?;

    Ok(format!(
        r#"import bpy
import math
import os
from mathutils import Vector

INPUT_PATH = {in_lit}
OUTPUT_PNG = {out_lit}
IN_EXT = "{in_ext}"

bpy.ops.wm.read_factory_settings(use_empty=True)

if IN_EXT in ("glb", "gltf"):
    bpy.ops.import_scene.gltf(filepath=INPUT_PATH)
elif IN_EXT == "fbx":
    bpy.ops.import_scene.fbx(filepath=INPUT_PATH)
elif IN_EXT == "obj":
    if hasattr(bpy.ops.wm, "obj_import"):
        bpy.ops.wm.obj_import(filepath=INPUT_PATH)
    else:
        bpy.ops.import_scene.obj(filepath=INPUT_PATH)
elif IN_EXT == "blend":
    bpy.ops.wm.open_mainfile(filepath=INPUT_PATH)

meshes = [obj for obj in bpy.context.scene.objects if obj.type == "MESH"]
min_v = Vector((1e9, 1e9, 1e9))
max_v = Vector((-1e9, -1e9, -1e9))
for obj in meshes:
    for corner in obj.bound_box:
        world_pt = obj.matrix_world @ Vector(corner)
        min_v.x = min(min_v.x, world_pt.x)
        min_v.y = min(min_v.y, world_pt.y)
        min_v.z = min(min_v.z, world_pt.z)
        max_v.x = max(max_v.x, world_pt.x)
        max_v.y = max(max_v.y, world_pt.y)
        max_v.z = max(max_v.z, world_pt.z)

if not meshes:
    center = Vector((0.0, 0.0, 0.0))
    radius = 1.0
else:
    center = (min_v + max_v) * 0.5
    radius = max((max_v - min_v).length * 0.5, 0.25)

scene = bpy.context.scene
scene.render.resolution_x = {width}
scene.render.resolution_y = {height}
scene.render.resolution_percentage = 100
scene.render.image_settings.file_format = "PNG"
scene.render.filepath = OUTPUT_PNG

for engine_name in ("BLENDER_EEVEE_NEXT", "BLENDER_EEVEE", "CYCLES"):
    try:
        scene.render.engine = engine_name
        break
    except Exception:
        pass

if scene.world is None:
    scene.world = bpy.data.worlds.new("DexStudioWorld")
scene.world.use_nodes = False
scene.world.color = (0.05, 0.07, 0.10)

cam_data = bpy.data.cameras.new("DexCamera")
cam_obj = bpy.data.objects.new("DexCamera", cam_data)
scene.collection.objects.link(cam_obj)
scene.camera = cam_obj

dist = radius * 2.8
cam_obj.location = center + Vector((dist * 0.72, -dist * 0.85, dist * 0.55))
direction = center - cam_obj.location
cam_obj.rotation_euler = direction.to_track_quat("-Z", "Y").to_euler()

def add_light(name, energy, offset):
    ldata = bpy.data.lights.new(name=name, type="AREA")
    ldata.energy = energy * max(radius * radius, 1.0)
    ldata.size = radius * 2.0
    lobj = bpy.data.objects.new(name=name, object_data=ldata)
    scene.collection.objects.link(lobj)
    lobj.location = center + offset
    lobj.rotation_euler = (center - lobj.location).to_track_quat("-Z", "Y").to_euler()

add_light("KeyLight", 240.0, Vector((dist, -dist * 0.6, dist * 0.9)))
add_light("FillLight", 90.0, Vector((-dist, -dist * 0.5, dist * 0.4)))
add_light("RimLight", 140.0, Vector((0.0, dist, dist * 0.8)))

os.makedirs(os.path.dirname(os.path.abspath(OUTPUT_PNG)), exist_ok=True)
bpy.ops.render.render(write_still=True)
"#
    ))
}

async fn run_blender_script(script_body: &str, op_label: &str) -> Result<(), ThreeDError> {
    let blender_exe = detect_blender_executable().await.ok_or_else(|| {
        ThreeDError::BackendUnavailable {
            kind: "blender".to_string(),
            reason: "Blender executable not found. Install Blender (e.g. `brew install --cask blender`) or set `BLENDER_BIN`.".to_string(),
        }
    })?;

    let tmp_dir = harness_core::resolve_dex_subpath(BLENDER_SCRIPTS_TMP_DIR);
    tokio::fs::create_dir_all(&tmp_dir)
        .await
        .map_err(|e| ThreeDError::Io {
            path: tmp_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let script_path = tmp_dir.join(format!(
        "dex_blender_{}_{}.py",
        op_label,
        current_epoch_millis()
    ));
    tokio::fs::write(&script_path, script_body)
        .await
        .map_err(|e| ThreeDError::Io {
            path: script_path.to_string_lossy().to_string(),
            source: e,
        })?;

    let args = build_blender_cli_args(&script_path);
    debug!(exe = ?blender_exe, args = ?args, "Executing headless Blender script");

    let mut blender_cmd = Command::new(&blender_exe);
    harness_core::apply_enriched_path(&mut blender_cmd);
    let run_result = tokio::time::timeout(
        Duration::from_secs(BLENDER_TIMEOUT_SECS),
        blender_cmd.args(&args).output(),
    )
    .await;

    // Always clean up temporary Python script
    let _ = tokio::fs::remove_file(&script_path).await;

    let output = run_result
        .map_err(|_| ThreeDError::Timeout {
            operation: format!("blender {op_label}"),
            timeout_secs: BLENDER_TIMEOUT_SECS,
        })?
        .map_err(|e| ThreeDError::Subprocess {
            command: blender_exe.to_string_lossy().to_string(),
            message: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let details = if !stderr.is_empty() { stderr } else { stdout };
        return Err(ThreeDError::Subprocess {
            command: "blender".to_string(),
            message: format!("Exited with status {}: {details}", output.status),
        });
    }

    Ok(())
}

/// Converts a 3D mesh between `.glb`, `.gltf`, `.fbx`, `.obj`, and `.blend` via headless Blender,
/// with optional polygon decimation (`0.05..=1.0`) and transform application.
pub async fn blender_convert_mesh(
    input_path: &Path,
    output_path: &Path,
    decimate_ratio: Option<f32>,
    apply_transforms: bool,
    event_bus: Option<&EventBus>,
) -> Result<BlenderConvertResult, ThreeDError> {
    if !tokio::fs::try_exists(input_path).await.unwrap_or(false) {
        return Err(ThreeDError::InvalidInput {
            reason: format!("Input 3D model does not exist: '{}'", input_path.display()),
        });
    }

    let script = build_convert_script(input_path, output_path, decimate_ratio, apply_transforms)?;
    run_blender_script(&script, "convert").await?;

    let file_size_bytes = tokio::fs::metadata(output_path)
        .await
        .map_err(|e| ThreeDError::Io {
            path: output_path.to_string_lossy().to_string(),
            source: e,
        })?
        .len();

    let out_ext = output_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mesh_report = if matches!(out_ext.as_str(), "glb" | "gltf" | "obj") {
        inspect_mesh(output_path).await.ok()
    } else {
        None
    };

    if let Some(bus) = event_bus {
        let dec_label = decimate_ratio
            .map(|r| format!(" (decimate {r:.2})"))
            .unwrap_or_default();
        bus.publish(DexEvent::MediaGenerated {
            kind: "3d".to_string(),
            path: output_path.to_string_lossy().to_string(),
            prompt: format!("Blender convert{dec_label} from {}", input_path.display()),
            duration_secs: None,
        });
    }

    info!(output = ?output_path, size = file_size_bytes, "Blender mesh conversion completed");

    Ok(BlenderConvertResult {
        input_path: input_path.to_path_buf(),
        output_path: output_path.to_path_buf(),
        decimate_ratio,
        applied_transforms: apply_transforms,
        file_size_bytes,
        mesh_report,
    })
}

/// Renders an auto-framed studio preview PNG of a 3D model via headless Blender.
pub async fn blender_render_preview(
    input_path: &Path,
    output_png: Option<&Path>,
    width: u32,
    height: u32,
    event_bus: Option<&EventBus>,
) -> Result<BlenderPreviewResult, ThreeDError> {
    if !tokio::fs::try_exists(input_path).await.unwrap_or(false) {
        return Err(ThreeDError::InvalidInput {
            reason: format!("Input 3D model does not exist: '{}'", input_path.display()),
        });
    }

    let dest = match output_png {
        Some(p) => harness_core::resolve_safe_path(p),
        None => {
            let dir = harness_core::resolve_dex_subpath(DEFAULT_3D_PREVIEWS_DIR);
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| ThreeDError::Io {
                    path: dir.to_string_lossy().to_string(),
                    source: e,
                })?;
            let stem = input_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("mesh");
            dir.join(format!("{stem}_preview_{}.png", current_epoch_millis()))
        }
    };

    let script = build_render_preview_script(input_path, &dest, width, height)?;
    run_blender_script(&script, "preview").await?;

    let file_size_bytes = tokio::fs::metadata(&dest)
        .await
        .map_err(|e| ThreeDError::Io {
            path: dest.to_string_lossy().to_string(),
            source: e,
        })?
        .len();

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerated {
            kind: "image".to_string(),
            path: dest.to_string_lossy().to_string(),
            prompt: format!("Blender 3D studio preview of {}", input_path.display()),
            duration_secs: None,
        });
    }

    Ok(BlenderPreviewResult {
        input_path: input_path.to_path_buf(),
        preview_png: dest,
        width,
        height,
        file_size_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blender_convert_script_and_cli_args_safety() {
        // Invalid decimate ratio rejected
        assert!(build_convert_script(
            Path::new("a.glb"),
            Path::new("b.fbx"),
            Some(0.01),
            true
        )
        .is_err());
        assert!(build_convert_script(
            Path::new("a.glb"),
            Path::new("b.fbx"),
            Some(1.5),
            true
        )
        .is_err());

        // Unsupported extension rejected
        assert!(build_convert_script(
            Path::new("a.txt"),
            Path::new("b.glb"),
            None,
            false
        )
        .is_err());

        // Valid script safely JSON-escapes quotes in paths
        let script = build_convert_script(
            Path::new("/tmp/hero \"v1\"/model.glb"),
            Path::new("/tmp/out/model_lod1.fbx"),
            Some(0.35),
            true,
        )
        .expect("should build valid blender convert script");

        assert!(script.contains(r#"INPUT_PATH = "/tmp/hero \"v1\"/model.glb""#));
        assert!(script.contains(r#"OUTPUT_PATH = "/tmp/out/model_lod1.fbx""#));
        assert!(script.contains("DECIMATE_RATIO = 0.3500"));
        assert!(script.contains("APPLY_TRANSFORMS = True"));
        assert!(script.contains("export_scene.fbx"));

        let args = build_blender_cli_args(Path::new("/tmp/dex_script.py"));
        assert_eq!(
            args,
            vec![
                "--background",
                "--factory-startup",
                "--python-exit-code",
                "1",
                "--python",
                "/tmp/dex_script.py",
            ]
        );
    }

    #[test]
    fn test_blender_preview_script_generation() {
        let script = build_render_preview_script(
            Path::new("mesh.glb"),
            Path::new(".dex/media/3d/previews/mesh.png"),
            800,
            600,
        )
        .expect("should build preview script");

        assert!(script.contains("resolution_x = 800"));
        assert!(script.contains("resolution_y = 600"));
        assert!(script.contains("to_track_quat"));
        assert!(script.contains("bpy.ops.render.render(write_still=True)"));

        assert!(build_render_preview_script(Path::new("mesh.glb"), Path::new("out.png"), 0, 600)
            .is_err());
    }
}
