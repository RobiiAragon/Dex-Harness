pub mod browser;
pub mod desktop;
pub mod error;

pub use browser::{
    BrowserClickArgs, BrowserClickTool, BrowserExtractDomArgs, BrowserExtractDomTool,
    BrowserNavigateArgs, BrowserNavigateTool, BrowserScreenshotArgs, BrowserScreenshotTool,
    BrowserSession, BrowserTypeArgs, BrowserTypeTool,
};
pub use desktop::{
    get_primary_monitor_resolution, DesktopKeyboardArgs, DesktopKeyboardTool, DesktopMouseArgs,
    DesktopMouseTool, DesktopScreenshotArgs, DesktopScreenshotTool, MouseAction,
};
pub use error::ComputerError;
