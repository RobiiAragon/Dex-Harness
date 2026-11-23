use std::path::{Path, PathBuf};
use std::sync::Arc;
use anyhow::{Context, Result};
use harness_3d::{
    blender_convert_mesh, blender_render_preview, generate_3d_mesh, inspect_mesh, list_3d_models,
    unity_export_asset, unity_inspect_project, write_sample_cube_glb, Gen3dBackend, Gen3dRequest,
    MeshReport, DEFAULT_3D_DIR,
};
use harness_core::EventBus;
use harness_extensions::ExtensionSupervisor;

fn print_mesh_report(report: &MeshReport) {
    let kb = (report.file_size_bytes as f64) / 1024.0;
    println!("🧊 3D Mesh Report: {}", report.path);
    println!("   Format         : {} ({:.2} KB)", report.format.to_uppercase(), kb);
    println!("   Meshes / Nodes : {} / {}", report.mesh_count, report.node_count);
    println!("   Vertices       : {}", report.vertex_count);
    println!("   Triangles      : {}", report.triangle_count);
    println!(
        "   Attributes     : Normals={} | UVs={} | Animations={}",
        if report.has_normals { "Yes" } else { "No" },
        if report.has_uvs { "Yes" } else { "No" },
        report.animation_count
    );
    if report.material_names.is_empty() {
        println!("   Materials      : (none)");
    } else {
        println!("   Materials      : {}", report.material_names.join(", "));
    }
    if let Some(ref bb) = report.bounding_box {
        println!(
            "   Bounding Box   : min=[{:.3}, {:.3}, {:.3}] max=[{:.3}, {:.3}, {:.3}]",
            bb.min[0], bb.min[1], bb.min[2], bb.max[0], bb.max[1], bb.max[2]
        );
        println!(
            "   Dimensions     : {:.3} × {:.3} × {:.3} units",
            bb.dimensions[0], bb.dimensions[1], bb.dimensions[2]
        );
    }
    if report.game_ready_warnings.is_empty() {
        println!("   Game-Ready     : ✅ Clean (no real-time warnings)");
    } else {
        println!("   Game-Ready Warnings:");
        for w in &report.game_ready_warnings {
            println!("     ⚠️  {w}");
        }
    }
}

pub async fn inspect_model(path: &str) -> Result<()> {
    let report = inspect_mesh(Path::new(path))
        .await
        .with_context(|| format!("Failed to inspect 3D mesh '{path}'"))?;
    print_mesh_report(&report);
    Ok(())
}

pub async fn create_sample(out: Option<&str>) -> Result<()> {
    let dest = match out {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(DEFAULT_3D_DIR).join("sample_cube.glb"),
    };
    let saved = write_sample_cube_glb(&dest)
        .await
        .with_context(|| format!("Failed to generate sample cube GLB at '{}'", dest.display()))?;
    println!("✅ Generated sample glTF 2.0 binary cube: {}", saved.display());
    let report = inspect_mesh(&saved)
        .await
        .context("Failed to inspect generated sample cube")?;
    print_mesh_report(&report);
    Ok(())
}

pub async fn generate_model(
    image: Option<String>,
    prompt: Option<String>,
    backend: &str,
    event_bus: EventBus,
) -> Result<()> {
    let parsed_backend = Gen3dBackend::from_str_loose(backend);
    println!("🧊 Generating 3D mesh (backend: {:?})...", parsed_backend);
    let supervisor = Arc::new(ExtensionSupervisor::new(event_bus.clone()));
    let req = Gen3dRequest {
        image_path: image,
        prompt,
        backend: parsed_backend,
        texture_resolution: 1024,
        remove_background: true,
    };

    let res = generate_3d_mesh(req, Some(supervisor), Some(&event_bus))
        .await
        .context("3D mesh generation failed")?;
    println!("✅ Generated 3D mesh via {}: {}", res.backend, res.path.display());
    print_mesh_report(&res.report);
    Ok(())
}

pub async fn convert_model(
    input: &str,
    out: &str,
    decimate: Option<f32>,
    event_bus: EventBus,
) -> Result<()> {
    println!("🌀 Converting '{input}' -> '{out}' via headless Blender...");
    let res = blender_convert_mesh(
        Path::new(input),
        Path::new(out),
        decimate,
        true,
        Some(&event_bus),
    )
    .await
    .context("Blender conversion failed")?;
    println!(
        "✅ Converted to {} ({:.1} KB)",
        res.output_path.display(),
        (res.file_size_bytes as f64) / 1024.0
    );
    if let Some(ref report) = res.mesh_report {
        print_mesh_report(report);
    }
    Ok(())
}

pub async fn preview_model(input: &str, out: Option<&str>, event_bus: EventBus) -> Result<()> {
    println!("📷 Rendering studio preview of '{input}' via headless Blender...");
    let res = blender_render_preview(
        Path::new(input),
        out.map(Path::new),
        768,
        768,
        Some(&event_bus),
    )
    .await
    .context("Blender studio preview render failed")?;
    println!(
        "✅ Rendered {}x{} studio preview ({:.1} KB) -> {}",
        res.width,
        res.height,
        (res.file_size_bytes as f64) / 1024.0,
        res.preview_png.display()
    );
    Ok(())
}

pub async fn unity_inspect(project_dir: &str) -> Result<()> {
    let report = unity_inspect_project(Path::new(project_dir))
        .await
        .with_context(|| format!("Failed to inspect Unity project '{project_dir}'"))?;

    println!("🎮 Unity Project Report: {}", report.project_dir);
    println!(
        "   Unity Version   : {}",
        report.unity_version.as_deref().unwrap_or("unknown")
    );
    println!("   Render Pipeline : {}", report.render_pipeline);
    println!("   C# Scripts      : {}", report.csharp_script_count);
    println!("   Packages        : {}", report.packages.len());
    if report.scenes.is_empty() {
        println!("   Scenes          : (none found)");
    } else {
        println!("   Scenes ({}):", report.scenes.len());
        for scene in &report.scenes {
            println!("     - {scene}");
        }
    }
    Ok(())
}

pub async fn unity_export(
    mesh_path: &str,
    project_dir: &str,
    subdir: Option<&str>,
    overwrite: bool,
) -> Result<()> {
    let res = unity_export_asset(Path::new(mesh_path), Path::new(project_dir), subdir, overwrite)
        .await
        .context("Failed to export 3D asset to Unity project")?;

    println!(
        "✅ Exported {} file(s) to Unity folder: {}",
        res.copied_files.len(),
        res.destination_dir.display()
    );
    for f in &res.copied_files {
        println!("   - {}", f.display());
    }
    Ok(())
}

pub async fn list_models() -> Result<()> {
    let items = list_3d_models()
        .await
        .context("Failed to list 3D models in .dex/media/3d/")?;

    println!("🧊 3D Models Library (.dex/media/3d/):");
    if items.is_empty() {
        println!("   (No 3D models found yet. Try `dex 3d sample` to generate a sample cube!)");
        return Ok(());
    }

    for item in items {
        let kb = (item.size_bytes as f64) / 1024.0;
        let tris = item
            .triangle_count
            .map(|t| format!("{t} tris"))
            .unwrap_or_else(|| "n/a".to_string());
        println!(
            "   🧊 [{}] {} ({:.1} KB, {}) -> {}",
            item.format.to_uppercase(),
            item.filename,
            kb,
            tris,
            item.path
        );
    }
    Ok(())
}
