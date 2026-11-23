use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::ScreenshotParams;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, info, instrument};

use harness_core::{Tool, ToolError};

/// Shared session manager for browser automation.
#[derive(Clone)]
pub struct BrowserSession {
    _browser: Arc<Mutex<Option<Arc<Browser>>>>,
    active_page: Arc<Mutex<Option<chromiumoxide::Page>>>,
}

impl Default for BrowserSession {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserSession {
    pub fn new() -> Self {
        Self {
            _browser: Arc::new(Mutex::new(None)),
            active_page: Arc::new(Mutex::new(None)),
        }
    }

    /// Retrieves or launches the shared browser instance.
    pub async fn get_or_launch(&self, headless: bool) -> Result<chromiumoxide::Page, ToolError> {
        let mut b_guard = self._browser.lock().await;
        let mut p_guard = self.active_page.lock().await;

        if let Some(ref p) = *p_guard {
            return Ok(p.clone());
        }

        let mut config_builder = BrowserConfig::builder();
        if !headless {
            config_builder = config_builder.with_head();
        }

        let config = config_builder
            .no_sandbox()
            .build()
            .map_err(|e| ToolError::ExecutionFailed {
                name: "browser".to_string(),
                message: format!("Failed to configure browser: {e}"),
            })?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: "browser".to_string(),
                message: format!("Failed to launch Chrome/Chromium: {e}. Ensure Chrome is installed on the host."),
            })?;

        // Spawn browser handler loop in background
        tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if let Err(e) = h {
                    debug!("Browser handler event: {:?}", e);
                }
            }
        });

        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: "browser".to_string(),
                message: format!("Failed to create initial browser page: {e}"),
            })?;

        *b_guard = Some(Arc::new(browser));
        *p_guard = Some(page.clone());

        Ok(page)
    }

    pub async fn get_active_page(&self) -> Result<chromiumoxide::Page, ToolError> {
        let guard = self.active_page.lock().await;
        guard.clone().ok_or_else(|| ToolError::ExecutionFailed {
            name: "browser".to_string(),
            message: "No active browser page open. Call 'browser_navigate' first.".to_string(),
        })
    }
}

// =========================================================================
// 1. Browser Navigate Tool
// =========================================================================

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserNavigateArgs {
    /// Target URL to navigate to (e.g. "https://crates.io").
    pub url: String,
    /// Whether to run in headless mode. Defaults to true.
    pub headless: Option<bool>,
}

pub struct BrowserNavigateTool {
    session: BrowserSession,
}

impl BrowserNavigateTool {
    pub fn new(session: BrowserSession) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for BrowserNavigateTool {
    fn name(&self) -> &str {
        "browser_navigate"
    }

    fn description(&self) -> &str {
        "Navigates to a URL in a real Chrome/Chromium browser instance. Supports headless or visible window modes."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(BrowserNavigateArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "URL to open" },
                "headless": { "type": "boolean", "description": "Run headless (default true)" }
            },
            "required": ["url"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: BrowserNavigateArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let headless = args.headless.unwrap_or(true);
        let page = self.session.get_or_launch(headless).await?;

        page.goto(&args.url).await.map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: format!("Failed to navigate to '{}': {e}", args.url),
        })?;

        let title = page.get_title().await.unwrap_or_default().unwrap_or_else(|| "Untitled".to_string());
        let current_url = page.url().await.unwrap_or_default().unwrap_or_else(|| args.url.clone());

        info!(url = %current_url, title = %title, "Browser navigation complete");

        Ok(format!(
            "Navigated successfully:\n- URL: {}\n- Title: {}",
            current_url, title
        ))
    }
}

// =========================================================================
// 2. Browser Extract DOM Tool
// =========================================================================

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserExtractDomArgs {
    /// Optional CSS selector to extract from (defaults to full body).
    pub selector: Option<String>,
}

pub struct BrowserExtractDomTool {
    session: BrowserSession,
}

impl BrowserExtractDomTool {
    pub fn new(session: BrowserSession) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for BrowserExtractDomTool {
    fn name(&self) -> &str {
        "browser_extract_dom"
    }

    fn description(&self) -> &str {
        "Extracts rendered text content from the current web page, automatically stripping scripts, stylesheets, and HTML tags for optimal token efficiency."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(BrowserExtractDomArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "Optional CSS selector to scope extraction" }
            }
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: BrowserExtractDomArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let page = self.session.get_active_page().await?;

        let js_expr = match args.selector {
            Some(ref sel) => format!(
                "document.querySelector('{sel}') ? document.querySelector('{sel}').innerText : 'Element not found'"
            ),
            None => "document.body.innerText".to_string(),
        };

        let result: String = page
            .evaluate(js_expr)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!("Failed to extract DOM text: {e}"),
            })?
            .into_value()
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!("Failed to convert evaluation result: {e}"),
            })?;

        let lines: Vec<&str> = result
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();

        let clean_text = lines.join("\n");
        let preview = if clean_text.len() > 8000 {
            format!("{}...\n[Truncated: {} total chars]", &clean_text[..8000], clean_text.len())
        } else {
            clean_text
        };

        Ok(preview)
    }
}

// =========================================================================
// 3. Browser Click & Type Tools
// =========================================================================

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserClickArgs {
    /// CSS selector of the element to click (e.g. "button#submit", "a.nav-link").
    pub selector: String,
}

pub struct BrowserClickTool {
    session: BrowserSession,
}

impl BrowserClickTool {
    pub fn new(session: BrowserSession) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for BrowserClickTool {
    fn name(&self) -> &str {
        "browser_click"
    }

    fn description(&self) -> &str {
        "Clicks an element on the active web page identified by a CSS selector."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(BrowserClickArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "CSS selector to click" }
            },
            "required": ["selector"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: BrowserClickArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let page = self.session.get_active_page().await?;
        let element = page
            .find_element(&args.selector)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!("Element '{}' not found: {e}", args.selector),
            })?;

        element.click().await.map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: format!("Failed to click element '{}': {e}", args.selector),
        })?;

        Ok(format!("Clicked element '{}'", args.selector))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserTypeArgs {
    /// CSS selector of the input/textarea element to type into.
    pub selector: String,
    /// Text to enter into the element.
    pub text: String,
}

pub struct BrowserTypeTool {
    session: BrowserSession,
}

impl BrowserTypeTool {
    pub fn new(session: BrowserSession) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for BrowserTypeTool {
    fn name(&self) -> &str {
        "browser_type"
    }

    fn description(&self) -> &str {
        "Types text into an input field or textarea identified by a CSS selector."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(BrowserTypeArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "Target CSS selector" },
                "text": { "type": "string", "description": "Text to enter" }
            },
            "required": ["selector", "text"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: BrowserTypeArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let page = self.session.get_active_page().await?;
        let element = page
            .find_element(&args.selector)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!("Element '{}' not found: {e}", args.selector),
            })?;

        element.type_str(&args.text).await.map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: format!("Failed to type into element '{}': {e}", args.selector),
        })?;

        Ok(format!("Typed text into element '{}'", args.selector))
    }
}

// =========================================================================
// 4. Browser Screenshot Tool
// =========================================================================

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserScreenshotArgs {
    /// Optional destination path. Defaults to .dex/screenshots/
    pub destination_path: Option<String>,
}

pub struct BrowserScreenshotTool {
    session: BrowserSession,
}

impl BrowserScreenshotTool {
    pub fn new(session: BrowserSession) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for BrowserScreenshotTool {
    fn name(&self) -> &str {
        "browser_screenshot"
    }

    fn description(&self) -> &str {
        "Captures a screenshot of the active browser webpage and saves it to .dex/screenshots/."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(BrowserScreenshotArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "destination_path": { "type": "string", "description": "Target file path" }
            }
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: BrowserScreenshotArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let page = self.session.get_active_page().await?;
        let screenshot_data = page
            .screenshot(ScreenshotParams::builder().full_page(false).build())
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!("Failed to take webpage screenshot: {e}"),
            })?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let out_path: PathBuf = match args.destination_path {
            Some(p) => harness_core::resolve_safe_path(p),
            None => {
                let dir = harness_core::resolve_dex_subpath("screenshots");
                let _ = tokio::fs::create_dir_all(&dir).await;
                dir.join(format!("browser_{timestamp}.png"))
            }
        };

        if let Some(parent) = out_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        tokio::fs::write(&out_path, screenshot_data).await.map_err(|e| ToolError::Io {
            name: self.name().to_string(),
            message: format!("Failed to save screenshot file: {e}"),
        })?;

        Ok(format!(
            "Webpage screenshot saved successfully at '{}'",
            out_path.display()
        ))
    }
}
