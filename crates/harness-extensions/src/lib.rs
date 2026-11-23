pub mod catalog;
pub mod error;
pub mod manifest;
pub mod mcp_bridge;
pub mod supervisor;

pub use catalog::{builtin_catalog, find_catalog_recipe};
pub use error::ExtensionError;
pub use manifest::{
    ExtensionCategory, ExtensionKind, ExtensionManifest, ExtensionRegistry,
    DEFAULT_IDLE_TIMEOUT_SECS,
};
pub use mcp_bridge::{connect_mcp_extension, register_enabled_mcp_extensions, ExtensionMcpTool};
pub use supervisor::{ExtensionRuntimeInfo, ExtensionStatus, ExtensionSupervisor};
