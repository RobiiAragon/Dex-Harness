pub mod blender;
pub mod error;
pub mod gen3d;
pub mod mesh_ops;
pub mod tools;
pub mod unity;

pub use blender::{
    blender_convert_mesh, blender_render_preview, build_blender_cli_args, build_convert_script,
    build_render_preview_script, check_blender_status, detect_blender_executable,
    BlenderConvertResult, BlenderPreviewResult, BlenderStatus,
};
pub use error::ThreeDError;
pub use gen3d::{generate_3d_mesh, Gen3dBackend, Gen3dRequest, Gen3dResult};
pub use mesh_ops::{
    evaluate_game_ready_warnings, inspect_mesh, list_3d_models, list_3d_models_in_dir,
    write_sample_cube_glb, BoundingBox, MeshReport, StoredModel3dItem, DEFAULT_3D_DIR,
    DEFAULT_3D_PREVIEWS_DIR, REALTIME_TRIANGLE_BUDGET,
};
pub use tools::{
    register_3d_tools, BlenderConvertMeshArgs, BlenderConvertMeshTool, BlenderRenderPreviewArgs,
    BlenderRenderPreviewTool, MeshGenerate3dArgs, MeshGenerate3dTool, MeshInspectArgs,
    MeshInspectTool, UnityExportAssetArgs, UnityExportAssetTool, UnityInspectProjectArgs,
    UnityInspectProjectTool,
};
pub use unity::{
    unity_export_asset, unity_inspect_project, validate_safe_unity_subdir, UnityExportResult,
    UnityProjectReport, DEFAULT_UNITY_SUBDIR,
};
