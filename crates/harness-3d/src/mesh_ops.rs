use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::ThreeDError;

pub const DEFAULT_3D_DIR: &str = ".dex/media/3d";
pub const DEFAULT_3D_PREVIEWS_DIR: &str = ".dex/media/3d/previews";
pub const REALTIME_TRIANGLE_BUDGET: u64 = 50_000;

/// Axis-aligned bounding box of a 3D mesh in model space.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BoundingBox {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub dimensions: [f32; 3],
}

/// Complete pure-Rust structural and game-readiness report for a 3D mesh file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshReport {
    pub path: String,
    pub format: String,
    pub file_size_bytes: u64,
    pub mesh_count: usize,
    pub node_count: usize,
    pub vertex_count: u64,
    pub triangle_count: u64,
    pub material_names: Vec<String>,
    pub has_normals: bool,
    pub has_uvs: bool,
    pub animation_count: usize,
    pub bounding_box: Option<BoundingBox>,
    pub game_ready_warnings: Vec<String>,
}

/// Entry describing a stored 3D model discovered in `.dex/media/3d/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredModel3dItem {
    pub filename: String,
    pub path: String,
    pub format: String,
    pub size_bytes: u64,
    pub triangle_count: Option<u64>,
    pub vertex_count: Option<u64>,
}

/// Evaluates game-engine readiness warnings for a mesh report.
pub fn evaluate_game_ready_warnings(
    vertex_count: u64,
    triangle_count: u64,
    has_normals: bool,
    has_uvs: bool,
    bounding_box: Option<&BoundingBox>,
) -> Vec<String> {
    let mut warnings = Vec::new();

    if vertex_count == 0 || triangle_count == 0 {
        warnings.push("Mesh contains no renderable geometry (0 vertices or 0 triangles).".to_string());
        return warnings;
    }

    if triangle_count > REALTIME_TRIANGLE_BUDGET {
        warnings.push(format!(
            "High polygon count ({triangle_count} triangles exceeds real-time budget of {REALTIME_TRIANGLE_BUDGET}). Consider decimating with `blender_convert_mesh` for game LODs."
        ));
    }

    if !has_normals {
        warnings.push(
            "Missing vertex normals (NORMAL attribute): lighting/shading may appear faceted or dark in Unity/WebGL."
                .to_string(),
        );
    }

    if !has_uvs {
        warnings.push(
            "Missing UV coordinates (TEXCOORD_0): PBR image textures will not map cleanly."
                .to_string(),
        );
    }

    if let Some(bbox) = bounding_box {
        let max_dim = bbox.dimensions[0]
            .max(bbox.dimensions[1])
            .max(bbox.dimensions[2]);
        if max_dim > 100.0 {
            warnings.push(format!(
                "Very large model scale (max dimension {max_dim:.2} units). glTF/Unity expect 1 unit = 1 meter."
            ));
        } else if max_dim > 0.0 && max_dim < 0.01 {
            warnings.push(format!(
                "Very tiny model scale (max dimension {max_dim:.5} units). Verify export scale before importing into Unity."
            ));
        }
    }

    warnings
}

/// Inspects a `.glb`, `.gltf`, or `.obj` mesh natively in pure Rust inside `spawn_blocking`.
pub async fn inspect_mesh(path: &Path) -> Result<MeshReport, ThreeDError> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Err(ThreeDError::InvalidInput {
            reason: format!("3D model file does not exist: '{}'", path.display()),
        });
    }

    let owned = path.to_path_buf();
    let display = path.to_string_lossy().to_string();

    tokio::task::spawn_blocking(move || inspect_mesh_blocking(&owned))
        .await
        .map_err(|e| ThreeDError::Gltf {
            path: display,
            message: format!("Mesh inspection worker failed: {e}"),
        })?
}

fn inspect_mesh_blocking(path: &Path) -> Result<MeshReport, ThreeDError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let file_size_bytes = std::fs::metadata(path)
        .map_err(|e| ThreeDError::Io {
            path: path.to_string_lossy().to_string(),
            source: e,
        })?
        .len();

    match ext.as_str() {
        "glb" | "gltf" => inspect_gltf_blocking(path, &ext, file_size_bytes),
        "obj" => inspect_obj_blocking(path, file_size_bytes),
        other => Err(ThreeDError::InvalidInput {
            reason: format!(
                "Unsupported format '.{other}' for native pure-Rust inspection. Supported: .glb, .gltf, .obj (use `blender_convert_mesh` to convert .fbx/.blend to .glb first)."
            ),
        }),
    }
}

fn update_bounds(min: &mut [f32; 3], max: &mut [f32; 3], has_bounds: &mut bool, p: [f32; 3]) {
    if !p[0].is_finite() || !p[1].is_finite() || !p[2].is_finite() {
        return;
    }
    if !*has_bounds {
        *min = p;
        *max = p;
        *has_bounds = true;
    } else {
        for axis in 0..3 {
            if p[axis] < min[axis] {
                min[axis] = p[axis];
            }
            if p[axis] > max[axis] {
                max[axis] = p[axis];
            }
        }
    }
}

fn build_bbox(min: [f32; 3], max: [f32; 3], has_bounds: bool) -> Option<BoundingBox> {
    if !has_bounds {
        return None;
    }
    Some(BoundingBox {
        min,
        max,
        dimensions: [max[0] - min[0], max[1] - min[1], max[2] - min[2]],
    })
}

fn inspect_gltf_blocking(
    path: &Path,
    format: &str,
    file_size_bytes: u64,
) -> Result<MeshReport, ThreeDError> {
    let (document, buffers, _images) = gltf::import(path).map_err(|e| ThreeDError::Gltf {
        path: path.to_string_lossy().to_string(),
        message: e.to_string(),
    })?;

    let mesh_count = document.meshes().len();
    let node_count = document.nodes().len();
    let animation_count = document.animations().len();

    let mut materials_set = BTreeSet::new();
    for mat in document.materials() {
        let name = mat
            .name()
            .filter(|n| !n.trim().is_empty())
            .map(String::from)
            .unwrap_or_else(|| {
                mat.index()
                    .map(|idx| format!("Material_{idx}"))
                    .unwrap_or_else(|| "DefaultMaterial".to_string())
            });
        materials_set.insert(name);
    }

    let mut vertex_count: u64 = 0;
    let mut triangle_count: u64 = 0;
    let mut total_primitives: usize = 0;
    let mut primitives_with_normals: usize = 0;
    let mut primitives_with_uvs: usize = 0;
    let mut min = [0.0f32; 3];
    let mut max = [0.0f32; 3];
    let mut has_bounds = false;

    for mesh in document.meshes() {
        for primitive in mesh.primitives() {
            total_primitives += 1;

            if primitive.get(&gltf::Semantic::Normals).is_some() {
                primitives_with_normals += 1;
            }
            if primitive.get(&gltf::Semantic::TexCoords(0)).is_some() {
                primitives_with_uvs += 1;
            }

            let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));

            let mut prim_vertices: u64 = 0;
            if let Some(positions) = reader.read_positions() {
                for pos in positions {
                    prim_vertices += 1;
                    update_bounds(&mut min, &mut max, &mut has_bounds, pos);
                }
            } else {
                let bb = primitive.bounding_box();
                update_bounds(&mut min, &mut max, &mut has_bounds, bb.min);
                update_bounds(&mut min, &mut max, &mut has_bounds, bb.max);
                if let Some(pos_acc) = primitive.get(&gltf::Semantic::Positions) {
                    prim_vertices = pos_acc.count() as u64;
                }
            }
            vertex_count = vertex_count.saturating_add(prim_vertices);

            let elem_count: u64 = if let Some(indices) = reader.read_indices() {
                indices.into_u32().count() as u64
            } else {
                prim_vertices
            };

            let prim_tris = match primitive.mode() {
                gltf::mesh::Mode::Triangles => elem_count / 3,
                gltf::mesh::Mode::TriangleStrip | gltf::mesh::Mode::TriangleFan => {
                    elem_count.saturating_sub(2)
                }
                _ => 0,
            };
            triangle_count = triangle_count.saturating_add(prim_tris);
        }
    }

    let has_normals = total_primitives > 0 && primitives_with_normals == total_primitives;
    let has_uvs = total_primitives > 0 && primitives_with_uvs == total_primitives;
    let bounding_box = build_bbox(min, max, has_bounds);
    let game_ready_warnings = evaluate_game_ready_warnings(
        vertex_count,
        triangle_count,
        has_normals,
        has_uvs,
        bounding_box.as_ref(),
    );

    debug!(
        path = ?path,
        vertices = vertex_count,
        triangles = triangle_count,
        "Inspected glTF/GLB mesh"
    );

    Ok(MeshReport {
        path: path.to_string_lossy().to_string(),
        format: format.to_string(),
        file_size_bytes,
        mesh_count,
        node_count,
        vertex_count,
        triangle_count,
        material_names: materials_set.into_iter().collect(),
        has_normals,
        has_uvs,
        animation_count,
        bounding_box,
        game_ready_warnings,
    })
}

fn inspect_obj_blocking(path: &Path, file_size_bytes: u64) -> Result<MeshReport, ThreeDError> {
    let file = std::fs::File::open(path).map_err(|e| ThreeDError::Io {
        path: path.to_string_lossy().to_string(),
        source: e,
    })?;
    let reader = BufReader::new(file);

    let mut vertex_count: u64 = 0;
    let mut triangle_count: u64 = 0;
    let mut normal_count: u64 = 0;
    let mut uv_count: u64 = 0;
    let mut object_count: usize = 0;
    let mut materials_set = BTreeSet::new();
    let mut min = [0.0f32; 3];
    let mut max = [0.0f32; 3];
    let mut has_bounds = false;

    for line_res in reader.lines() {
        let line = line_res.map_err(|e| ThreeDError::Io {
            path: path.to_string_lossy().to_string(),
            source: e,
        })?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("v ") {
            vertex_count += 1;
            let mut coords = rest.split_whitespace();
            if let (Some(xs), Some(ys), Some(zs)) = (coords.next(), coords.next(), coords.next()) {
                if let (Ok(x), Ok(y), Ok(z)) = (xs.parse::<f32>(), ys.parse::<f32>(), zs.parse::<f32>())
                {
                    update_bounds(&mut min, &mut max, &mut has_bounds, [x, y, z]);
                }
            }
        } else if trimmed.starts_with("vn ") {
            normal_count += 1;
        } else if trimmed.starts_with("vt ") {
            uv_count += 1;
        } else if let Some(rest) = trimmed.strip_prefix("f ") {
            let verts = rest.split_whitespace().count() as u64;
            if verts >= 3 {
                // Fan triangulation for triangles, quads, and n-gons
                triangle_count = triangle_count.saturating_add(verts - 2);
            }
        } else if trimmed.starts_with("o ") || trimmed.starts_with("g ") {
            object_count += 1;
        } else if let Some(mat_name) = trimmed.strip_prefix("usemtl ") {
            let clean = mat_name.trim();
            if !clean.is_empty() {
                materials_set.insert(clean.to_string());
            }
        }
    }

    let mesh_count = if vertex_count > 0 {
        object_count.max(1)
    } else {
        0
    };
    let has_normals = normal_count > 0;
    let has_uvs = uv_count > 0;
    let bounding_box = build_bbox(min, max, has_bounds);
    let game_ready_warnings = evaluate_game_ready_warnings(
        vertex_count,
        triangle_count,
        has_normals,
        has_uvs,
        bounding_box.as_ref(),
    );

    Ok(MeshReport {
        path: path.to_string_lossy().to_string(),
        format: "obj".to_string(),
        file_size_bytes,
        mesh_count,
        node_count: mesh_count,
        vertex_count,
        triangle_count,
        material_names: materials_set.into_iter().collect(),
        has_normals,
        has_uvs,
        animation_count: 0,
        bounding_box,
        game_ready_warnings,
    })
}

/// Synthesizes a valid glTF 2.0 binary `.glb` 1m × 1m × 1m cube with outward normals, UVs,
/// 36 indices (12 triangles), and a PBR material in pure Rust.
pub async fn write_sample_cube_glb(path: &Path) -> Result<PathBuf, ThreeDError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ThreeDError::Io {
                    path: parent.to_string_lossy().to_string(),
                    source: e,
                })?;
        }
    }

    let glb_bytes = build_sample_cube_glb_bytes()?;
    tokio::fs::write(path, &glb_bytes)
        .await
        .map_err(|e| ThreeDError::Io {
            path: path.to_string_lossy().to_string(),
            source: e,
        })?;

    Ok(path.to_path_buf())
}

fn build_sample_cube_glb_bytes() -> Result<Vec<u8>, ThreeDError> {
    // 6 faces * 4 vertices = 24 vertices with crisp per-face normals and UVs
    #[rustfmt::skip]
    let positions: [[f32; 3]; 24] = [
        // +Z (front)
        [-0.5, -0.5,  0.5], [ 0.5, -0.5,  0.5], [ 0.5,  0.5,  0.5], [-0.5,  0.5,  0.5],
        // -Z (back)
        [ 0.5, -0.5, -0.5], [-0.5, -0.5, -0.5], [-0.5,  0.5, -0.5], [ 0.5,  0.5, -0.5],
        // +Y (top)
        [-0.5,  0.5,  0.5], [ 0.5,  0.5,  0.5], [ 0.5,  0.5, -0.5], [-0.5,  0.5, -0.5],
        // -Y (bottom)
        [-0.5, -0.5, -0.5], [ 0.5, -0.5, -0.5], [ 0.5, -0.5,  0.5], [-0.5, -0.5,  0.5],
        // +X (right)
        [ 0.5, -0.5,  0.5], [ 0.5, -0.5, -0.5], [ 0.5,  0.5, -0.5], [ 0.5,  0.5,  0.5],
        // -X (left)
        [-0.5, -0.5, -0.5], [-0.5, -0.5,  0.5], [-0.5,  0.5,  0.5], [-0.5,  0.5, -0.5],
    ];

    #[rustfmt::skip]
    let normals: [[f32; 3]; 24] = [
        [ 0.0,  0.0,  1.0], [ 0.0,  0.0,  1.0], [ 0.0,  0.0,  1.0], [ 0.0,  0.0,  1.0],
        [ 0.0,  0.0, -1.0], [ 0.0,  0.0, -1.0], [ 0.0,  0.0, -1.0], [ 0.0,  0.0, -1.0],
        [ 0.0,  1.0,  0.0], [ 0.0,  1.0,  0.0], [ 0.0,  1.0,  0.0], [ 0.0,  1.0,  0.0],
        [ 0.0, -1.0,  0.0], [ 0.0, -1.0,  0.0], [ 0.0, -1.0,  0.0], [ 0.0, -1.0,  0.0],
        [ 1.0,  0.0,  0.0], [ 1.0,  0.0,  0.0], [ 1.0,  0.0,  0.0], [ 1.0,  0.0,  0.0],
        [-1.0,  0.0,  0.0], [-1.0,  0.0,  0.0], [-1.0,  0.0,  0.0], [-1.0,  0.0,  0.0],
    ];

    #[rustfmt::skip]
    let uvs: [[f32; 2]; 24] = [
        [0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0],
        [0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0],
        [0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0],
        [0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0],
        [0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0],
        [0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0],
    ];

    #[rustfmt::skip]
    let indices: [u16; 36] = [
         0,  1,  2,   0,  2,  3,
         4,  5,  6,   4,  6,  7,
         8,  9, 10,   8, 10, 11,
        12, 13, 14,  12, 14, 15,
        16, 17, 18,  16, 18, 19,
        20, 21, 22,  20, 22, 23,
    ];

    let mut bin = Vec::with_capacity(288 + 288 + 192 + 72);
    let pos_offset = bin.len();
    for v in &positions {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let pos_len = bin.len() - pos_offset;

    let norm_offset = bin.len();
    for n in &normals {
        for c in n {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let norm_len = bin.len() - norm_offset;

    let uv_offset = bin.len();
    for uv in &uvs {
        for c in uv {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let uv_len = bin.len() - uv_offset;

    let idx_offset = bin.len();
    for i in &indices {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    let idx_len = bin.len() - idx_offset;

    // Pad BIN chunk to 4-byte alignment with 0x00
    while !bin.len().is_multiple_of(4) {
        bin.push(0x00);
    }

    let gltf_json = serde_json::json!({
        "asset": {
            "version": "2.0",
            "generator": "Dex-Harness harness-3d"
        },
        "scene": 0,
        "scenes": [{ "name": "DexSampleScene", "nodes": [0] }],
        "nodes": [{ "name": "DexSampleCube", "mesh": 0 }],
        "materials": [{
            "name": "DexCyanMetal",
            "pbrMetallicRoughness": {
                "baseColorFactor": [0.22, 0.77, 0.81, 1.0],
                "metallicFactor": 0.25,
                "roughnessFactor": 0.45
            }
        }],
        "meshes": [{
            "name": "DexCubeMesh",
            "primitives": [{
                "attributes": {
                    "POSITION": 0,
                    "NORMAL": 1,
                    "TEXCOORD_0": 2
                },
                "indices": 3,
                "material": 0,
                "mode": 4
            }]
        }],
        "buffers": [{ "byteLength": bin.len() }],
        "bufferViews": [
            { "buffer": 0, "byteOffset": pos_offset, "byteLength": pos_len, "target": 34962 },
            { "buffer": 0, "byteOffset": norm_offset, "byteLength": norm_len, "target": 34962 },
            { "buffer": 0, "byteOffset": uv_offset, "byteLength": uv_len, "target": 34962 },
            { "buffer": 0, "byteOffset": idx_offset, "byteLength": idx_len, "target": 34963 }
        ],
        "accessors": [
            {
                "bufferView": 0,
                "componentType": 5126,
                "count": 24,
                "type": "VEC3",
                "min": [-0.5, -0.5, -0.5],
                "max": [0.5, 0.5, 0.5]
            },
            {
                "bufferView": 1,
                "componentType": 5126,
                "count": 24,
                "type": "VEC3"
            },
            {
                "bufferView": 2,
                "componentType": 5126,
                "count": 24,
                "type": "VEC2"
            },
            {
                "bufferView": 3,
                "componentType": 5123,
                "count": 36,
                "type": "SCALAR"
            }
        ]
    });

    let mut json_bytes = serde_json::to_vec(&gltf_json).map_err(|e| ThreeDError::Json {
        context: "serialize sample cube glTF JSON".to_string(),
        source: e,
    })?;
    // Pad JSON chunk to 4-byte alignment with ASCII spaces (0x20) per glTF 2.0 spec
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }

    let total_length = 12usize + 8 + json_bytes.len() + 8 + bin.len();
    let total_length_u32 = u32::try_from(total_length).map_err(|_| ThreeDError::InvalidInput {
        reason: "Sample GLB exceeds 4 GiB".to_string(),
    })?;
    let json_len_u32 = u32::try_from(json_bytes.len()).map_err(|_| ThreeDError::InvalidInput {
        reason: "JSON chunk exceeds u32".to_string(),
    })?;
    let bin_len_u32 = u32::try_from(bin.len()).map_err(|_| ThreeDError::InvalidInput {
        reason: "BIN chunk exceeds u32".to_string(),
    })?;

    let mut glb = Vec::with_capacity(total_length);
    // Header: magic "glTF", version 2, total length
    glb.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&total_length_u32.to_le_bytes());
    // Chunk 0: JSON
    glb.extend_from_slice(&json_len_u32.to_le_bytes());
    glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    glb.extend_from_slice(&json_bytes);
    // Chunk 1: BIN
    glb.extend_from_slice(&bin_len_u32.to_le_bytes());
    glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
    glb.extend_from_slice(&bin);

    Ok(glb)
}

/// Scans `.dex/media/3d/` and returns all stored 3D models with optional triangle/vertex metrics.
pub async fn list_3d_models() -> Result<Vec<StoredModel3dItem>, ThreeDError> {
    let dir = harness_core::resolve_dex_subpath(DEFAULT_3D_DIR);
    list_3d_models_in_dir(&dir).await
}

/// Scans a specific directory for 3D models (`.glb`, `.gltf`, `.obj`, `.fbx`, `.blend`).
pub async fn list_3d_models_in_dir(dir: &Path) -> Result<Vec<StoredModel3dItem>, ThreeDError> {
    if !tokio::fs::try_exists(dir).await.unwrap_or(false) {
        return Ok(Vec::new());
    }

    let mut rd = tokio::fs::read_dir(dir).await.map_err(|e| ThreeDError::Io {
        path: dir.to_string_lossy().to_string(),
        source: e,
    })?;

    let mut items = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !matches!(ext.as_str(), "glb" | "gltf" | "obj" | "fbx" | "blend") {
            continue;
        }
        let size_bytes = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
        let filename = entry.file_name().to_string_lossy().to_string();

        let (triangle_count, vertex_count) = if matches!(ext.as_str(), "glb" | "gltf" | "obj") {
            match inspect_mesh(&path).await {
                Ok(report) => (Some(report.triangle_count), Some(report.vertex_count)),
                Err(_) => (None, None),
            }
        } else {
            (None, None)
        };

        items.push(StoredModel3dItem {
            filename,
            path: path.to_string_lossy().to_string(),
            format: ext,
            size_bytes,
            triangle_count,
            vertex_count,
        });
    }

    items.sort_by(|a, b| b.filename.cmp(&a.filename));
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sample_cube_glb_generation_and_inspection() {
        let temp_dir =
            std::env::temp_dir().join(format!("dex_3d_cube_test_{}", std::process::id()));
        let glb_path = temp_dir.join("sample_cube.glb");

        write_sample_cube_glb(&glb_path)
            .await
            .expect("should write valid GLB cube");

        let report = inspect_mesh(&glb_path)
            .await
            .expect("gltf crate should inspect sample cube GLB cleanly");

        assert_eq!(report.format, "glb");
        assert_eq!(report.mesh_count, 1);
        assert_eq!(report.node_count, 1);
        assert_eq!(report.vertex_count, 24);
        assert_eq!(report.triangle_count, 12);
        assert!(report.has_normals);
        assert!(report.has_uvs);
        assert_eq!(report.animation_count, 0);
        assert_eq!(report.material_names, vec!["DexCyanMetal".to_string()]);
        assert!(report.game_ready_warnings.is_empty());

        let bbox = report.bounding_box.expect("bounding box should be computed");
        assert_eq!(bbox.min, [-0.5, -0.5, -0.5]);
        assert_eq!(bbox.max, [0.5, 0.5, 0.5]);
        assert_eq!(bbox.dimensions, [1.0, 1.0, 1.0]);

        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    #[tokio::test]
    async fn test_obj_inspection_quad_triangulation_and_materials() {
        let temp_dir =
            std::env::temp_dir().join(format!("dex_3d_obj_test_{}", std::process::id()));
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .expect("create temp dir");
        let obj_path = temp_dir.join("quad.obj");
        let obj_content = "\
o TestPlane
usemtl FloorMat
v 0.0 0.0 0.0
v 2.0 0.0 0.0
v 2.0 0.0 2.0
v 0.0 0.0 2.0
vn 0.0 1.0 0.0
vt 0.0 0.0
f 1/1/1 2/1/1 3/1/1 4/1/1
";
        tokio::fs::write(&obj_path, obj_content)
            .await
            .expect("write obj");

        let report = inspect_mesh(&obj_path).await.expect("inspect obj");
        assert_eq!(report.format, "obj");
        assert_eq!(report.vertex_count, 4);
        assert_eq!(report.triangle_count, 2);
        assert!(report.has_normals);
        assert!(report.has_uvs);
        assert_eq!(report.material_names, vec!["FloorMat".to_string()]);
        let bbox = report.bounding_box.expect("bbox");
        assert_eq!(bbox.dimensions, [2.0, 0.0, 2.0]);

        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    #[test]
    fn test_game_ready_warnings_thresholds_and_attributes() {
        let huge_bbox = BoundingBox {
            min: [0.0, 0.0, 0.0],
            max: [250.0, 10.0, 10.0],
            dimensions: [250.0, 10.0, 10.0],
        };
        let warnings = evaluate_game_ready_warnings(80_000, 75_000, false, false, Some(&huge_bbox));
        assert_eq!(warnings.len(), 4);
        assert!(warnings.iter().any(|w| w.contains("75000 triangles")));
        assert!(warnings.iter().any(|w| w.contains("normals")));
        assert!(warnings.iter().any(|w| w.contains("UV coordinates")));
        assert!(warnings.iter().any(|w| w.contains("Very large model scale")));

        let empty_warnings = evaluate_game_ready_warnings(0, 0, false, false, None);
        assert_eq!(empty_warnings.len(), 1);
        assert!(empty_warnings[0].contains("no renderable geometry"));
    }
}
