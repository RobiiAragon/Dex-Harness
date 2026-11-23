use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::error::ThreeDError;

pub const DEFAULT_UNITY_SUBDIR: &str = "Assets/DexGenerated";

/// Structural overview of a Unity project read directly from disk (no Editor required).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnityProjectReport {
    pub project_dir: String,
    pub unity_version: Option<String>,
    pub render_pipeline: String,
    pub scenes: Vec<String>,
    pub csharp_script_count: usize,
    pub packages: Vec<String>,
}

/// Outcome of exporting a 3D mesh (and adjacent textures/materials) into a Unity project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnityExportResult {
    pub destination_dir: PathBuf,
    pub exported_model_path: PathBuf,
    pub copied_files: Vec<PathBuf>,
}

/// Inspects a Unity project directory (`Assets/`, `ProjectSettings/ProjectVersion.txt`,
/// `Packages/manifest.json`) to identify Unity version, render pipeline (`URP`/`HDRP`/`Built-in`),
/// `.unity` scenes, and C# script count.
pub async fn unity_inspect_project(project_dir: &Path) -> Result<UnityProjectReport, ThreeDError> {
    let assets_dir = project_dir.join("Assets");
    if !tokio::fs::try_exists(&assets_dir).await.unwrap_or(false) {
        return Err(ThreeDError::InvalidInput {
            reason: format!(
                "'{}' is not a valid Unity project directory (missing 'Assets/' folder).",
                project_dir.display()
            ),
        });
    }

    // 1. Parse Unity version from ProjectSettings/ProjectVersion.txt
    let version_file = project_dir.join("ProjectSettings/ProjectVersion.txt");
    let unity_version = if tokio::fs::try_exists(&version_file).await.unwrap_or(false) {
        tokio::fs::read_to_string(&version_file)
            .await
            .ok()
            .and_then(|content| {
                content.lines().find_map(|line| {
                    line.trim()
                        .strip_prefix("m_EditorVersion:")
                        .map(|v| v.trim().to_string())
                        .filter(|v| !v.is_empty())
                })
            })
    } else {
        None
    };

    // 2. Parse Packages/manifest.json for render pipeline & installed packages
    let manifest_file = project_dir.join("Packages/manifest.json");
    let mut packages = Vec::new();
    let mut render_pipeline = "Built-in".to_string();

    if tokio::fs::try_exists(&manifest_file).await.unwrap_or(false) {
        let raw = tokio::fs::read_to_string(&manifest_file)
            .await
            .map_err(|e| ThreeDError::Io {
                path: manifest_file.to_string_lossy().to_string(),
                source: e,
            })?;
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| ThreeDError::Json {
                context: format!("parse Unity manifest '{}'", manifest_file.display()),
                source: e,
            })?;

        if let Some(deps) = parsed.get("dependencies").and_then(|v| v.as_object()) {
            let mut has_hdrp = false;
            let mut has_urp = false;
            for pkg_name in deps.keys() {
                packages.push(pkg_name.clone());
                if pkg_name == "com.unity.render-pipelines.high-definition" {
                    has_hdrp = true;
                } else if pkg_name == "com.unity.render-pipelines.universal" {
                    has_urp = true;
                }
            }
            packages.sort();
            if has_hdrp {
                render_pipeline = "HDRP".to_string();
            } else if has_urp {
                render_pipeline = "URP".to_string();
            }
        }
    }

    // 3. Walk Assets/ recursively in spawn_blocking to list .unity scenes and count .cs scripts
    let owned_project = project_dir.to_path_buf();
    let owned_assets = assets_dir.clone();
    let (scenes, csharp_script_count) = tokio::task::spawn_blocking(move || {
        walk_unity_assets_blocking(&owned_project, &owned_assets)
    })
    .await
    .map_err(|e| ThreeDError::Io {
        path: assets_dir.to_string_lossy().to_string(),
        source: std::io::Error::other(format!("Unity asset scan worker failed: {e}")),
    })??;

    Ok(UnityProjectReport {
        project_dir: project_dir.to_string_lossy().to_string(),
        unity_version,
        render_pipeline,
        scenes,
        csharp_script_count,
        packages,
    })
}

fn walk_unity_assets_blocking(
    project_root: &Path,
    assets_dir: &Path,
) -> Result<(Vec<String>, usize), ThreeDError> {
    let mut stack = vec![assets_dir.to_path_buf()];
    let mut scenes = Vec::new();
    let mut cs_count: usize = 0;

    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| ThreeDError::Io {
            path: dir.to_string_lossy().to_string(),
            source: e,
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ext.eq_ignore_ascii_case("unity") {
                    let rel = path
                        .strip_prefix(project_root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    scenes.push(rel);
                } else if ext.eq_ignore_ascii_case("cs") {
                    cs_count += 1;
                }
            }
        }
    }

    scenes.sort();
    Ok((scenes, cs_count))
}

/// Validates that a Unity destination subdirectory is strictly relative inside `Assets/`
/// with zero path traversal (`..`), absolute prefixes, or control characters.
pub fn validate_safe_unity_subdir(subdir: &str) -> Result<PathBuf, ThreeDError> {
    let trimmed = subdir.trim();
    if trimmed.is_empty() {
        return Err(ThreeDError::SecurityViolation {
            reason: "Unity target subdir cannot be empty".to_string(),
        });
    }
    if trimmed.contains('\0') || trimmed.contains('\n') || trimmed.contains('\r') {
        return Err(ThreeDError::SecurityViolation {
            reason: "Unity target subdir contains illegal control characters".to_string(),
        });
    }

    let p = Path::new(trimmed);
    if p.is_absolute() {
        return Err(ThreeDError::SecurityViolation {
            reason: format!("Absolute paths are not allowed for Unity subdir: '{trimmed}'"),
        });
    }

    let mut clean = PathBuf::new();
    let mut component_count = 0usize;
    for comp in p.components() {
        match comp {
            Component::Normal(seg) => {
                let s = seg.to_string_lossy();
                if component_count == 0 && s != "Assets" {
                    return Err(ThreeDError::SecurityViolation {
                        reason: format!(
                            "Unity export subdir must stay within 'Assets/' (got '{trimmed}')"
                        ),
                    });
                }
                clean.push(seg);
                component_count += 1;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ThreeDError::SecurityViolation {
                    reason: format!(
                        "Path traversal or root component rejected in Unity subdir: '{trimmed}'"
                    ),
                });
            }
        }
    }

    if component_count == 0 {
        return Err(ThreeDError::SecurityViolation {
            reason: "Unity target subdir resolved to empty path".to_string(),
        });
    }

    Ok(clean)
}

fn sanitize_asset_folder_name(mesh_path: &Path) -> Result<String, ThreeDError> {
    let stem = mesh_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim();
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() || clean == "." || clean == ".." {
        return Err(ThreeDError::InvalidInput {
            reason: format!(
                "Could not derive a safe asset name from '{}'",
                mesh_path.display()
            ),
        });
    }
    Ok(clean)
}

/// Copies a 3D model (`.glb`, `.gltf`, `.fbx`, `.obj`) and any matching adjacent textures/buffers
/// into `<project_dir>/<subdir>/<asset_name>/` with strict path-traversal and overwrite guards.
pub async fn unity_export_asset(
    mesh_path: &Path,
    project_dir: &Path,
    subdir: Option<&str>,
    overwrite: bool,
) -> Result<UnityExportResult, ThreeDError> {
    if !tokio::fs::try_exists(mesh_path).await.unwrap_or(false) {
        return Err(ThreeDError::InvalidInput {
            reason: format!("Source 3D mesh does not exist: '{}'", mesh_path.display()),
        });
    }

    let ext = mesh_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !matches!(ext.as_str(), "glb" | "gltf" | "fbx" | "obj") {
        return Err(ThreeDError::InvalidInput {
            reason: format!(
                "Unsupported Unity asset extension '.{ext}'. Expected .glb, .gltf, .fbx, or .obj"
            ),
        });
    }

    let assets_root = project_dir.join("Assets");
    if !tokio::fs::try_exists(&assets_root).await.unwrap_or(false) {
        return Err(ThreeDError::InvalidInput {
            reason: format!(
                "'{}' is not a valid Unity project (missing 'Assets/' folder).",
                project_dir.display()
            ),
        });
    }

    let safe_subdir =
        validate_safe_unity_subdir(subdir.unwrap_or(DEFAULT_UNITY_SUBDIR))?;
    let asset_name = sanitize_asset_folder_name(mesh_path)?;

    let canonical_assets = tokio::fs::canonicalize(&assets_root)
        .await
        .map_err(|e| ThreeDError::Io {
            path: assets_root.to_string_lossy().to_string(),
            source: e,
        })?;

    let dest_dir = project_dir.join(&safe_subdir).join(&asset_name);
    tokio::fs::create_dir_all(&dest_dir)
        .await
        .map_err(|e| ThreeDError::Io {
            path: dest_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let canonical_dest = tokio::fs::canonicalize(&dest_dir)
        .await
        .map_err(|e| ThreeDError::Io {
            path: dest_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    if !canonical_dest.starts_with(&canonical_assets) {
        return Err(ThreeDError::SecurityViolation {
            reason: format!(
                "Destination '{}' escapes Unity Assets root '{}'",
                canonical_dest.display(),
                canonical_assets.display()
            ),
        });
    }

    let primary_filename = mesh_path
        .file_name()
        .ok_or_else(|| ThreeDError::InvalidInput {
            reason: format!("Invalid source mesh filename: '{}'", mesh_path.display()),
        })?;
    let dest_model = canonical_dest.join(primary_filename);

    if tokio::fs::try_exists(&dest_model).await.unwrap_or(false) && !overwrite {
        return Err(ThreeDError::InvalidInput {
            reason: format!(
                "Destination asset already exists at '{}'. Pass `overwrite = true` to replace it.",
                dest_model.display()
            ),
        });
    }

    let mut files_to_copy: BTreeSet<PathBuf> = BTreeSet::new();
    files_to_copy.insert(mesh_path.to_path_buf());

    // Collect matching adjacent companion files (textures, .mtl, .bin)
    if let Some(src_dir) = mesh_path.parent() {
        let stem_lower = mesh_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        if let Ok(mut rd) = tokio::fs::read_dir(src_dir).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let p = entry.path();
                if p == mesh_path || !p.is_file() {
                    continue;
                }
                let f_ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let f_name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if matches!(
                    f_ext.as_str(),
                    "mtl" | "bin" | "png" | "jpg" | "jpeg" | "webp" | "tga" | "exr"
                ) && !stem_lower.is_empty()
                    && f_name.starts_with(&stem_lower)
                {
                    files_to_copy.insert(p);
                }
            }
        }

        // For .gltf files, also collect referenced local relative buffers and images
        if ext == "gltf" {
            if let Ok(raw) = tokio::fs::read_to_string(mesh_path).await {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
                    for section in ["buffers", "images"] {
                        if let Some(arr) = json.get(section).and_then(|v| v.as_array()) {
                            for item in arr {
                                if let Some(uri) = item.get("uri").and_then(|v| v.as_str()) {
                                    if !uri.starts_with("data:")
                                        && !uri.contains("://")
                                        && !uri.contains("..")
                                        && !Path::new(uri).is_absolute()
                                    {
                                        let candidate = src_dir.join(uri);
                                        if tokio::fs::try_exists(&candidate)
                                            .await
                                            .unwrap_or(false)
                                        {
                                            files_to_copy.insert(candidate);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let mut copied_files = Vec::with_capacity(files_to_copy.len());
    for src in files_to_copy {
        if let Some(fname) = src.file_name() {
            let target = canonical_dest.join(fname);
            if tokio::fs::try_exists(&target).await.unwrap_or(false) && !overwrite {
                continue;
            }
            tokio::fs::copy(&src, &target)
                .await
                .map_err(|e| ThreeDError::Io {
                    path: target.to_string_lossy().to_string(),
                    source: e,
                })?;
            copied_files.push(target);
        }
    }

    info!(
        dest = ?dest_model,
        copied = copied_files.len(),
        "Exported 3D asset to Unity project"
    );

    Ok(UnityExportResult {
        destination_dir: canonical_dest,
        exported_model_path: dest_model,
        copied_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_ops::write_sample_cube_glb;

    async fn create_mock_unity_project(root: &Path, pipeline_pkg: Option<&str>) {
        tokio::fs::create_dir_all(root.join("Assets/Scenes"))
            .await
            .expect("create scenes dir");
        tokio::fs::create_dir_all(root.join("Assets/Scripts"))
            .await
            .expect("create scripts dir");
        tokio::fs::create_dir_all(root.join("ProjectSettings"))
            .await
            .expect("create ProjectSettings");
        tokio::fs::create_dir_all(root.join("Packages"))
            .await
            .expect("create Packages");

        tokio::fs::write(
            root.join("ProjectSettings/ProjectVersion.txt"),
            "m_EditorVersion: 6000.0.23f1\nm_EditorVersionWithRevision: 6000.0.23f1 (abc)\n",
        )
        .await
        .expect("write version");

        let deps = match pipeline_pkg {
            Some(pkg) => serde_json::json!({
                "dependencies": {
                    pkg: "17.0.3",
                    "com.unity.textmeshpro": "3.0.6"
                }
            }),
            None => serde_json::json!({
                "dependencies": {
                    "com.unity.textmeshpro": "3.0.6"
                }
            }),
        };
        tokio::fs::write(
            root.join("Packages/manifest.json"),
            serde_json::to_string_pretty(&deps).expect("json"),
        )
        .await
        .expect("write manifest");

        tokio::fs::write(root.join("Assets/Scenes/Main.unity"), "%YAML 1.1\n")
            .await
            .expect("write scene");
        tokio::fs::write(
            root.join("Assets/Scripts/PlayerController.cs"),
            "public class PlayerController {}",
        )
        .await
        .expect("write cs 1");
        tokio::fs::write(
            root.join("Assets/Scripts/CameraRig.cs"),
            "public class CameraRig {}",
        )
        .await
        .expect("write cs 2");
    }

    #[tokio::test]
    async fn test_unity_inspect_urp_hdrp_and_builtin() {
        let base = std::env::temp_dir().join(format!("dex_unity_inspect_{}", std::process::id()));

        let urp_dir = base.join("urp_proj");
        create_mock_unity_project(&urp_dir, Some("com.unity.render-pipelines.universal")).await;
        let urp = unity_inspect_project(&urp_dir).await.expect("inspect URP");
        assert_eq!(urp.unity_version.as_deref(), Some("6000.0.23f1"));
        assert_eq!(urp.render_pipeline, "URP");
        assert_eq!(urp.scenes, vec!["Assets/Scenes/Main.unity".to_string()]);
        assert_eq!(urp.csharp_script_count, 2);

        let hdrp_dir = base.join("hdrp_proj");
        create_mock_unity_project(
            &hdrp_dir,
            Some("com.unity.render-pipelines.high-definition"),
        )
        .await;
        let hdrp = unity_inspect_project(&hdrp_dir)
            .await
            .expect("inspect HDRP");
        assert_eq!(hdrp.render_pipeline, "HDRP");

        let builtin_dir = base.join("builtin_proj");
        create_mock_unity_project(&builtin_dir, None).await;
        let builtin = unity_inspect_project(&builtin_dir)
            .await
            .expect("inspect Built-in");
        assert_eq!(builtin.render_pipeline, "Built-in");

        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn test_unity_export_anti_path_traversal_and_overwrite_guard() {
        // Security checks on subdir
        assert!(validate_safe_unity_subdir("../escape").is_err());
        assert!(validate_safe_unity_subdir("Assets/../../etc").is_err());
        assert!(validate_safe_unity_subdir("/tmp/evil").is_err());
        assert!(validate_safe_unity_subdir("ProjectSettings/Hack").is_err());
        assert!(validate_safe_unity_subdir("Assets/DexGenerated/Props").is_ok());

        let base = std::env::temp_dir().join(format!("dex_unity_export_{}", std::process::id()));
        let proj_dir = base.join("GameProject");
        create_mock_unity_project(&proj_dir, Some("com.unity.render-pipelines.universal")).await;

        let src_dir = base.join("src_models");
        let glb_path = src_dir.join("hero_cube.glb");
        write_sample_cube_glb(&glb_path)
            .await
            .expect("write sample glb");
        let tex_path = src_dir.join("hero_cube_albedo.png");
        tokio::fs::write(&tex_path, b"fake_png_bytes")
            .await
            .expect("write companion texture");

        // Reject traversal attempt at export call
        assert!(unity_export_asset(&glb_path, &proj_dir, Some("Assets/../../escape"), false)
            .await
            .is_err());

        // First export succeeds and copies companion texture
        let exported = unity_export_asset(&glb_path, &proj_dir, None, false)
            .await
            .expect("export to unity");
        assert!(tokio::fs::try_exists(&exported.exported_model_path)
            .await
            .unwrap_or(false));
        assert_eq!(exported.copied_files.len(), 2);

        // Second export with overwrite=false must fail
        assert!(unity_export_asset(&glb_path, &proj_dir, None, false)
            .await
            .is_err());

        // Third export with overwrite=true succeeds
        let overwritten = unity_export_asset(&glb_path, &proj_dir, None, true)
            .await
            .expect("overwrite export to unity");
        assert_eq!(overwritten.copied_files.len(), 2);

        let _ = tokio::fs::remove_dir_all(&base).await;
    }
}
