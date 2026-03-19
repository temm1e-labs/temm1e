//! Browser tool — stealth headless Chrome automation via Chrome DevTools Protocol.
//!
//! Provides the agent with browser actions: navigate, click, type, screenshot,
//! get page text, evaluate JavaScript, save/restore sessions. Each tool call
//! performs exactly one action — the agent chains actions across rounds.
//!
//! ## Stealth Features (v1.2)
//!
//! - Anti-detection Chrome launch flags (disable automation indicators)
//! - JavaScript patches injected via CDP before any page scripts run
//!   (navigator.webdriver, plugins, languages, chrome.runtime, WebGL)
//! - Session persistence via CDP cookie save/restore
//! - Configurable idle timeout for long-running authenticated sessions

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
};
use chromiumoxide::cdp::browser_protocol::network::{
    CookieParam, CookieSameSite, GetCookiesParams, SetCookiesParams, TimeSinceEpoch,
};
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use chromiumoxide::page::Page;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{
    PathAccess, Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput, ToolOutputImage,
};
use tokio::sync::Mutex;

/// Default idle timeout (seconds). Overridden by `ToolsConfig.browser_timeout_secs`.
const DEFAULT_IDLE_TIMEOUT_SECS: i64 = 300;

/// Directory under `~/.temm1e/` where browser session cookies are stored.
const SESSIONS_DIR: &str = "sessions";

/// Realistic user-agent string to avoid headless detection.
/// Uses a common Windows Chrome fingerprint.
const STEALTH_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36";

/// JavaScript patches injected via `Page.addScriptToEvaluateOnNewDocument` to
/// mask automation indicators. Runs before ANY page scripts execute.
const STEALTH_JS: &str = r#"
// 1. Hide navigator.webdriver
Object.defineProperty(navigator, 'webdriver', {
    get: () => undefined,
    configurable: true
});

// 2. Fake navigator.plugins (empty array is a bot signal)
Object.defineProperty(navigator, 'plugins', {
    get: () => {
        const arr = [
            { name: 'Chrome PDF Plugin', filename: 'internal-pdf-viewer', description: 'Portable Document Format', length: 1 },
            { name: 'Chrome PDF Viewer', filename: 'mhjfbmdgcfjbbpaeojofohoefgiehjai', description: '', length: 1 },
            { name: 'Native Client', filename: 'internal-nacl-plugin', description: '', length: 1 }
        ];
        arr.length = 3;
        return arr;
    },
    configurable: true
});

// 3. Fake navigator.languages
Object.defineProperty(navigator, 'languages', {
    get: () => ['en-US', 'en'],
    configurable: true
});

// 4. Hide chrome.runtime (automation indicator)
if (window.chrome) {
    const originalChrome = window.chrome;
    window.chrome = {
        ...originalChrome,
        runtime: undefined
    };
}

// 5. WebGL vendor/renderer spoofing (avoid headless fingerprint)
(function() {
    const getParameterOrig = WebGLRenderingContext.prototype.getParameter;
    WebGLRenderingContext.prototype.getParameter = function(param) {
        // UNMASKED_VENDOR_WEBGL
        if (param === 37445) return 'Intel Inc.';
        // UNMASKED_RENDERER_WEBGL
        if (param === 37446) return 'Intel Iris OpenGL Engine';
        return getParameterOrig.apply(this, arguments);
    };
    // Also patch WebGL2 if available
    if (typeof WebGL2RenderingContext !== 'undefined') {
        const getParameter2Orig = WebGL2RenderingContext.prototype.getParameter;
        WebGL2RenderingContext.prototype.getParameter = function(param) {
            if (param === 37445) return 'Intel Inc.';
            if (param === 37446) return 'Intel Iris OpenGL Engine';
            return getParameter2Orig.apply(this, arguments);
        };
    }
})();

// 6. Patch permissions query (headless returns "denied" for notifications)
(function() {
    const originalQuery = window.navigator.permissions.query;
    window.navigator.permissions.query = function(parameters) {
        if (parameters.name === 'notifications') {
            return Promise.resolve({ state: Notification.permission });
        }
        return originalQuery.apply(this, arguments);
    };
})();
"#;

/// Serializable cookie for session persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionCookie {
    name: String,
    value: String,
    domain: Option<String>,
    path: Option<String>,
    expires: Option<f64>,
    http_only: Option<bool>,
    secure: Option<bool>,
    same_site: Option<String>,
}

/// Manages a shared browser instance with one active page.
/// Always runs headless with stealth anti-detection patches.
pub struct BrowserTool {
    browser: Arc<Mutex<Option<Browser>>>,
    page: Arc<Mutex<Option<Page>>>,
    /// Unix timestamp of last browser action — used for idle auto-close.
    last_used: Arc<AtomicI64>,
    /// Idle timeout in seconds before auto-closing the browser.
    idle_timeout_secs: i64,
    /// Shutdown flag — signals the watchdog task to exit.
    shutdown: Arc<AtomicBool>,
    /// Handle to the idle watchdog task — aborted on shutdown.
    watchdog_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Handle to the CDP handler task — aborted when browser is closed.
    cdp_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Last screenshot image data — consumed by runtime for vision injection.
    last_image: Arc<std::sync::Mutex<Option<ToolOutputImage>>>,
}

impl Default for BrowserTool {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserTool {
    /// Create a new browser tool with default timeout (300s).
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_IDLE_TIMEOUT_SECS as u64)
    }

    /// Create a new browser tool with a custom idle timeout (in seconds).
    pub fn with_timeout(timeout_secs: u64) -> Self {
        let browser = Arc::new(Mutex::new(None));
        let page = Arc::new(Mutex::new(None));
        let last_used = Arc::new(AtomicI64::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let idle_timeout = timeout_secs as i64;
        let cdp_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> =
            Arc::new(Mutex::new(None));

        // Spawn idle auto-close watchdog — store handle for cleanup on drop.
        let watchdog_handle = {
            let browser = browser.clone();
            let page = page.clone();
            let last_used = last_used.clone();
            let shutdown = shutdown.clone();
            let cdp_handle = cdp_handle.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    let lu = last_used.load(Ordering::Relaxed);
                    if lu == 0 {
                        continue; // never used yet
                    }
                    let now = chrono::Utc::now().timestamp();
                    if now - lu > idle_timeout {
                        let mut b = browser.lock().await;
                        let mut p = page.lock().await;
                        if b.is_some() {
                            tracing::info!("Browser idle for {}s — auto-closing", now - lu);
                            *p = None;
                            *b = None;
                            // Abort the CDP handler so it doesn't linger.
                            if let Some(handle) = cdp_handle.lock().await.take() {
                                handle.abort();
                            }
                            last_used.store(0, Ordering::Relaxed);
                        }
                    }
                }
            })
        };

        Self {
            browser,
            page,
            last_used,
            idle_timeout_secs: idle_timeout,
            shutdown,
            watchdog_handle: Mutex::new(Some(watchdog_handle)),
            cdp_handle,
            last_image: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Signal the watchdog to stop and abort background task handles.
    fn signal_shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Abort the watchdog task immediately instead of waiting for the next 30s tick.
        if let Some(handle) = self.watchdog_handle.get_mut().take() {
            handle.abort();
        }
    }

    /// Close the browser and free resources.
    async fn close_browser(&self) -> String {
        let mut browser_guard = self.browser.lock().await;
        let mut page_guard = self.page.lock().await;
        if browser_guard.is_some() {
            *page_guard = None;
            *browser_guard = None;
            // Abort the CDP handler task so it doesn't linger after the browser exits.
            if let Some(handle) = self.cdp_handle.lock().await.take() {
                handle.abort();
            }
            self.last_used.store(0, Ordering::Relaxed);
            tracing::info!("Browser closed by agent");
            "Browser closed.".to_string()
        } else {
            "No browser was running.".to_string()
        }
    }

    /// Lazily launch the browser on first use, or relaunch if dead.
    /// Applies stealth flags and injects anti-detection patches.
    async fn ensure_browser(&self) -> Result<Page, Temm1eError> {
        let mut browser_guard = self.browser.lock().await;
        let mut page_guard = self.page.lock().await;

        // If we have a cached page, verify it's still alive with a quick probe
        if let Some(ref page) = *page_guard {
            match page.get_title().await {
                Ok(_) => return Ok(page.clone()),
                Err(_) => {
                    tracing::warn!("Browser connection lost — relaunching");
                    *page_guard = None;
                    *browser_guard = None;
                    // Abort the stale CDP handler from the dead browser.
                    if let Some(handle) = self.cdp_handle.lock().await.take() {
                        handle.abort();
                    }
                }
            }
        }

        // ── Stealth launch flags ─────────────────────────────────────
        let mut config_builder = BrowserConfig::builder();
        
        // Try to find Chrome executable on Windows
        #[cfg(target_os = "windows")]
        {
            let chrome_paths = [
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            ];
            
            for path in &chrome_paths {
                if std::path::Path::new(path).exists() {
                    config_builder = config_builder.chrome_executable(path);
                    tracing::debug!("Found Chrome at: {}", path);
                    break;
                }
            }
        }
        
        let config = config_builder
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--no-sandbox")
            .arg("--disable-dev-shm-usage")
            .arg("--disable-setuid-sandbox")
            // Anti-detection flags
            .arg("--disable-blink-features=AutomationControlled")
            .arg("--disable-infobars")
            .arg("--disable-background-timer-throttling")
            .arg("--disable-backgrounding-occluded-windows")
            .arg("--disable-renderer-backgrounding")
            .arg("--disable-ipc-flooding-protection")
            // Stability flags for Windows
            .arg("--disable-extensions")
            .arg("--disable-plugins")
            .arg("--disable-software-rasterizer")
            .arg("--disable-web-security")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(format!("--user-agent={}", STEALTH_USER_AGENT))
            .arg("--lang=en-US,en")
            // Realistic window size (1920x1080 is common)
            .window_size(1920, 1080)
            .build()
            .map_err(|e| Temm1eError::Tool(format!("Failed to build browser config: {}", e)))?;

        let (browser, mut handler) = Browser::launch(config).await.map_err(|e| {
            Temm1eError::Tool(format!(
                "Failed to launch browser. Is Chrome/Chromium installed? Error: {}",
                e
            ))
        })?;

        // Spawn the CDP handler — this MUST keep running for the browser to work.
        // Store the handle so we can abort it when the browser is closed.
        let cdp_handle = tokio::spawn(async move {
            loop {
                match handler.next().await {
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::debug!("CDP handler event error: {}", e);
                    }
                    None => {
                        tracing::debug!("CDP handler stream ended");
                        break;
                    }
                }
            }
        });
        *self.cdp_handle.lock().await = Some(cdp_handle);

        // Give the browser a moment to fully initialize
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| Temm1eError::Tool(format!("Failed to create page: {}", e)))?;

        // ── Inject anti-detection patches via CDP ────────────────────
        // This runs the JS BEFORE any page scripts on every new document.
        page.execute(AddScriptToEvaluateOnNewDocumentParams::new(STEALTH_JS))
            .await
            .map_err(|e| Temm1eError::Tool(format!("Failed to inject stealth patches: {}", e)))?;

        tracing::info!("Stealth patches injected via Page.addScriptToEvaluateOnNewDocument");

        // Wait for page to be ready
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        *browser_guard = Some(browser);
        *page_guard = Some(page.clone());
        self.last_used
            .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);

        tracing::info!(
            timeout_secs = self.idle_timeout_secs,
            "Browser launched (headless, stealth mode)"
        );
        Ok(page)
    }

    /// Save all browser cookies to a session file under `~/.temm1e/sessions/`.
    async fn save_session(&self, page: &Page, session_name: &str) -> Result<String, Temm1eError> {
        // Get all cookies via CDP (Network.getCookies with no URL filter = all cookies)
        let response = page
            .execute(GetCookiesParams::default())
            .await
            .map_err(|e| Temm1eError::Tool(format!("Failed to get cookies via CDP: {}", e)))?;

        let cookies: Vec<SessionCookie> = response
            .result
            .cookies
            .iter()
            .map(|c| SessionCookie {
                name: c.name.clone(),
                value: c.value.clone(),
                domain: Some(c.domain.clone()),
                path: Some(c.path.clone()),
                expires: Some(c.expires),
                http_only: Some(c.http_only),
                secure: Some(c.secure),
                same_site: c.same_site.as_ref().map(|s| s.as_ref().to_string()),
            })
            .collect();

        let cookie_count = cookies.len();

        // Serialize to JSON
        let json = serde_json::to_string_pretty(&cookies).map_err(|e| {
            Temm1eError::Tool(format!("Failed to serialize session cookies: {}", e))
        })?;

        // Write to ~/.temm1e/sessions/{name}.json
        let sessions_dir = sessions_dir()?;
        std::fs::create_dir_all(&sessions_dir).map_err(|e| {
            Temm1eError::Tool(format!("Failed to create sessions directory: {}", e))
        })?;

        let safe_name = sanitize_session_name(session_name);
        let path = sessions_dir.join(format!("{}.json", safe_name));

        // Write with restrictive permissions
        std::fs::write(&path, &json)
            .map_err(|e| Temm1eError::Tool(format!("Failed to write session file: {}", e)))?;

        // Set file permissions to owner-only (Unix)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&path, perms).map_err(|e| {
                Temm1eError::Tool(format!(
                    "Failed to restrict session file permissions: {}",
                    e
                ))
            })?;
        }

        tracing::info!(
            session = %safe_name,
            cookies = cookie_count,
            path = %path.display(),
            "Browser session saved"
        );

        Ok(format!(
            "Session '{}' saved: {} cookies → {}",
            safe_name,
            cookie_count,
            path.display()
        ))
    }

    /// Restore browser cookies from a session file under `~/.temm1e/sessions/`.
    async fn restore_session(
        &self,
        page: &Page,
        session_name: &str,
    ) -> Result<String, Temm1eError> {
        let sessions_dir = sessions_dir()?;
        let safe_name = sanitize_session_name(session_name);
        let path = sessions_dir.join(format!("{}.json", safe_name));

        if !path.exists() {
            return Err(Temm1eError::Tool(format!(
                "Session '{}' not found at {}",
                safe_name,
                path.display()
            )));
        }

        let json = std::fs::read_to_string(&path)
            .map_err(|e| Temm1eError::Tool(format!("Failed to read session file: {}", e)))?;

        let cookies: Vec<SessionCookie> = serde_json::from_str(&json)
            .map_err(|e| Temm1eError::Tool(format!("Failed to parse session file: {}", e)))?;

        let cookie_count = cookies.len();

        // Convert to CDP CookieParam and set via CDP
        let cookie_params: Vec<CookieParam> = cookies
            .iter()
            .map(|c| {
                let mut param = CookieParam::new(c.name.clone(), c.value.clone());
                if let Some(ref domain) = c.domain {
                    param.domain = Some(domain.clone());
                }
                if let Some(ref path) = c.path {
                    param.path = Some(path.clone());
                }
                if let Some(expires) = c.expires {
                    param.expires = Some(TimeSinceEpoch::new(expires));
                }
                if let Some(http_only) = c.http_only {
                    param.http_only = Some(http_only);
                }
                if let Some(secure) = c.secure {
                    param.secure = Some(secure);
                }
                if let Some(ref ss) = c.same_site {
                    if let Ok(parsed) = ss.parse::<CookieSameSite>() {
                        param.same_site = Some(parsed);
                    }
                }
                param
            })
            .collect();

        page.execute(SetCookiesParams::new(cookie_params))
            .await
            .map_err(|e| Temm1eError::Tool(format!("Failed to set cookies via CDP: {}", e)))?;

        tracing::info!(
            session = %safe_name,
            cookies = cookie_count,
            "Browser session restored"
        );

        Ok(format!(
            "Session '{}' restored: {} cookies loaded",
            safe_name, cookie_count
        ))
    }
}

/// Return the sessions directory path: `~/.temm1e/sessions/`.
fn sessions_dir() -> Result<std::path::PathBuf, Temm1eError> {
    dirs::home_dir()
        .map(|h| h.join(".temm1e").join(SESSIONS_DIR))
        .ok_or_else(|| Temm1eError::Tool("Cannot determine home directory".into()))
}

/// Sanitize a session name to a safe filename (alphanumeric, dots, dashes, underscores).
fn sanitize_session_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "default".to_string()
    } else {
        sanitized
    }
}

impl Drop for BrowserTool {
    fn drop(&mut self) {
        self.signal_shutdown();
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "Control a stealth Chrome browser to navigate websites, click elements, type text, \
         take screenshots, read page content, run JavaScript, and manage sessions. \
         Each call performs one action. Chain multiple calls for multi-step workflows.\n\n\
         Actions:\n\
         - navigate: Go to a URL\n\
         - click: Click an element by CSS selector\n\
         - click_at: Click at pixel coordinates (x, y) — use after screenshot for vision-based interaction\n\
         - type: Type text into an input field by CSS selector\n\
         - screenshot: Capture the page as a PNG — the image is returned for visual analysis\n\
         - get_text: Get the visible text content of the page\n\
         - evaluate: Execute JavaScript and return the result\n\
         - get_html: Get the raw HTML of the page or an element\n\
         - save_session: Save all cookies to a named session file\n\
         - restore_session: Restore cookies from a previously saved session\n\
         - close: Close the browser when done (auto-closes after idle timeout)\n\n\
         Vision workflow: screenshot → analyze image → click_at coordinates → repeat.\n\
         This bypasses Shadow DOM, anti-bot CSS tricks, and hidden elements.\n\n\
         The browser runs in stealth mode with anti-detection patches applied."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["navigate", "click", "click_at", "type", "screenshot", "get_text", "evaluate", "get_html", "save_session", "restore_session", "close"],
                    "description": "The browser action to perform"
                },
                "url": {
                    "type": "string",
                    "description": "URL to navigate to (for 'navigate' action)"
                },
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the target element (for 'click', 'type', 'get_html' actions)"
                },
                "x": {
                    "type": "number",
                    "description": "X pixel coordinate (for 'click_at' action). Get coordinates from analyzing a screenshot."
                },
                "y": {
                    "type": "number",
                    "description": "Y pixel coordinate (for 'click_at' action). Get coordinates from analyzing a screenshot."
                },
                "text": {
                    "type": "string",
                    "description": "Text to type (for 'type' action). For 'click_at', optional text to type after clicking."
                },
                "script": {
                    "type": "string",
                    "description": "JavaScript code to execute (for 'evaluate' action)"
                },
                "filename": {
                    "type": "string",
                    "description": "Screenshot filename (for 'screenshot' action, defaults to 'screenshot.png')"
                },
                "session_name": {
                    "type": "string",
                    "description": "Name for the session (for 'save_session'/'restore_session' actions, e.g. 'facebook', 'github')"
                }
            },
            "required": ["action"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: vec![
                PathAccess::ReadWrite("~/.temm1e/sessions".into()),
                PathAccess::Write(".".into()),
            ],
            network_access: vec!["*".to_string()],
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let action = input
            .arguments
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: action".into()))?;

        // Handle close before launching browser
        if action == "close" {
            let msg = self.close_browser().await;
            return Ok(ToolOutput {
                content: msg,
                is_error: false,
            });
        }

        let page = self.ensure_browser().await.map_err(|e| {
            tracing::error!(error = %e, "Failed to ensure browser");
            Temm1eError::Tool(format!(
                "Browser initialization failed: {}. \
                 This may be due to Chrome not being installed, insufficient permissions, \
                 or antivirus software blocking Chrome. \
                 Try: 1) Verify Chrome is installed, 2) Run as administrator, \
                 3) Check antivirus settings.",
                e
            ))
        })?;
        self.last_used
            .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);

        match action {
            "navigate" => {
                let url = input
                    .arguments
                    .get("url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'navigate' requires 'url' parameter".into())
                    })?;

                tracing::info!(url = %url, "Browser navigating (stealth)");
                page.goto(url)
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Navigation failed: {}", e)))?;

                // Wait for page to settle
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                let title = page
                    .get_title()
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Failed to get title: {}", e)))?
                    .unwrap_or_default();

                let current_url = page
                    .url()
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Failed to get URL: {}", e)))?
                    .map(|u| u.to_string())
                    .unwrap_or_default();

                Ok(ToolOutput {
                    content: format!("Navigated to: {}\nTitle: {}", current_url, title),
                    is_error: false,
                })
            }

            "click" => {
                let selector = input
                    .arguments
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'click' requires 'selector' parameter".into())
                    })?;

                tracing::info!(selector = %selector, "Browser clicking");
                let element = page.find_element(selector).await.map_err(|e| {
                    Temm1eError::Tool(format!(
                        "Element not found for selector '{}': {}",
                        selector, e
                    ))
                })?;

                element
                    .click()
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Click failed: {}", e)))?;

                // Wait for any navigation/updates
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                Ok(ToolOutput {
                    content: format!("Clicked element: {}", selector),
                    is_error: false,
                })
            }

            "click_at" => {
                let x = input
                    .arguments
                    .get("x")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'click_at' requires 'x' parameter".into())
                    })?;
                let y = input
                    .arguments
                    .get("y")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'click_at' requires 'y' parameter".into())
                    })?;

                tracing::info!(x = %x, y = %y, "Browser clicking at coordinates");

                // Move mouse to position
                let move_cmd = DispatchMouseEventParams {
                    r#type: DispatchMouseEventType::MouseMoved,
                    x,
                    y,
                    modifiers: None,
                    timestamp: None,
                    button: None,
                    buttons: None,
                    click_count: None,
                    force: None,
                    tangential_pressure: None,
                    tilt_x: None,
                    tilt_y: None,
                    twist: None,
                    delta_x: None,
                    delta_y: None,
                    pointer_type: None,
                };
                page.execute(move_cmd)
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Mouse move failed: {}", e)))?;

                // Mouse down
                let down_cmd = DispatchMouseEventParams {
                    r#type: DispatchMouseEventType::MousePressed,
                    x,
                    y,
                    modifiers: None,
                    timestamp: None,
                    button: Some(MouseButton::Left),
                    buttons: Some(1),
                    click_count: Some(1),
                    force: None,
                    tangential_pressure: None,
                    tilt_x: None,
                    tilt_y: None,
                    twist: None,
                    delta_x: None,
                    delta_y: None,
                    pointer_type: None,
                };
                page.execute(down_cmd)
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Mouse down failed: {}", e)))?;

                // Mouse up
                let up_cmd = DispatchMouseEventParams {
                    r#type: DispatchMouseEventType::MouseReleased,
                    x,
                    y,
                    modifiers: None,
                    timestamp: None,
                    button: Some(MouseButton::Left),
                    buttons: Some(0),
                    click_count: Some(1),
                    force: None,
                    tangential_pressure: None,
                    tilt_x: None,
                    tilt_y: None,
                    twist: None,
                    delta_x: None,
                    delta_y: None,
                    pointer_type: None,
                };
                page.execute(up_cmd)
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Mouse up failed: {}", e)))?;

                // Optional: type text after clicking (useful for input fields)
                if let Some(text) = input.arguments.get("text").and_then(|v| v.as_str()) {
                    // Small delay for focus
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    page.evaluate(format!(
                        "document.activeElement && document.execCommand('insertText', false, {})",
                        serde_json::to_string(text).unwrap_or_default()
                    ))
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Type after click failed: {}", e)))?;
                }

                // Wait for any navigation/updates
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                let mut result = format!("Clicked at ({}, {})", x, y);
                if let Some(text) = input.arguments.get("text").and_then(|v| v.as_str()) {
                    result.push_str(&format!(" and typed {} chars", text.len()));
                }

                Ok(ToolOutput {
                    content: result,
                    is_error: false,
                })
            }

            "type" => {
                let selector = input
                    .arguments
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'type' requires 'selector' parameter".into())
                    })?;
                let text = input
                    .arguments
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Temm1eError::Tool("'type' requires 'text' parameter".into()))?;

                tracing::info!(selector = %selector, "Browser typing");
                let element = page.find_element(selector).await.map_err(|e| {
                    Temm1eError::Tool(format!(
                        "Element not found for selector '{}': {}",
                        selector, e
                    ))
                })?;

                element
                    .click()
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Failed to focus element: {}", e)))?;

                element
                    .type_str(text)
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Type failed: {}", e)))?;

                Ok(ToolOutput {
                    content: format!("Typed {} chars into '{}'", text.len(), selector),
                    is_error: false,
                })
            }

            "screenshot" => {
                let filename = input
                    .arguments
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .unwrap_or("screenshot.png");

                // Sanitize filename
                let safe_name: String = filename
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
                    .collect();
                let safe_name = if safe_name.is_empty() {
                    "screenshot.png".to_string()
                } else {
                    safe_name
                };

                let save_path = ctx.workspace_path.join(&safe_name);

                tracing::info!(path = %save_path.display(), "Browser taking screenshot");
                let png_data = page
                    .screenshot(
                        chromiumoxide::page::ScreenshotParams::builder()
                            .full_page(true)
                            .build(),
                    )
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Screenshot failed: {}", e)))?;

                tokio::fs::write(&save_path, &png_data)
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Failed to save screenshot: {}", e)))?;

                // Store base64 image for vision injection by the runtime.
                // The runtime will check take_last_image() and feed it to the LLM.
                {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_data);
                    if let Ok(mut img) = self.last_image.lock() {
                        *img = Some(ToolOutputImage {
                            media_type: "image/png".to_string(),
                            data: b64,
                        });
                    }
                }

                Ok(ToolOutput {
                    content: format!(
                        "Screenshot captured: {} ({} bytes). The image is now visible to you for analysis. \
                         Use click_at with x,y coordinates to interact with elements you see.",
                        safe_name,
                        png_data.len(),
                    ),
                    is_error: false,
                })
            }

            "get_text" => {
                tracing::info!("Browser getting page text");

                let text: String = page
                    .evaluate("document.body.innerText")
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Failed to get text: {}", e)))?
                    .into_value()
                    .map_err(|e| Temm1eError::Tool(format!("Failed to parse text: {:?}", e)))?;

                // Truncate if too long (safe for multi-byte UTF-8)
                let max_bytes = 15_000;
                let truncated = if text.len() > max_bytes {
                    let boundary = text
                        .char_indices()
                        .map(|(i, _)| i)
                        .take_while(|&i| i <= max_bytes)
                        .last()
                        .unwrap_or(0);
                    format!(
                        "{}...\n\n[Truncated — {} total bytes]",
                        &text[..boundary],
                        text.len()
                    )
                } else {
                    text
                };

                Ok(ToolOutput {
                    content: truncated,
                    is_error: false,
                })
            }

            "evaluate" => {
                let script = input
                    .arguments
                    .get("script")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'evaluate' requires 'script' parameter".into())
                    })?;

                tracing::info!("Browser evaluating JavaScript");
                let result: serde_json::Value = page
                    .evaluate(script)
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("JS evaluation failed: {}", e)))?
                    .into_value()
                    .map_err(|e| {
                        Temm1eError::Tool(format!("Failed to parse JS result: {:?}", e))
                    })?;

                let content = match result {
                    serde_json::Value::String(s) => s,
                    other => serde_json::to_string_pretty(&other).unwrap_or_default(),
                };

                Ok(ToolOutput {
                    content,
                    is_error: false,
                })
            }

            "get_html" => {
                let selector = input.arguments.get("selector").and_then(|v| v.as_str());

                tracing::info!(selector = ?selector, "Browser getting HTML");

                let html: String = if let Some(sel) = selector {
                    let _element = page.find_element(sel).await.map_err(|e| {
                        Temm1eError::Tool(format!(
                            "Element not found for selector '{}': {}",
                            sel, e
                        ))
                    })?;
                    let escaped = serde_json::to_string(sel).unwrap_or_default();
                    let script = format!("document.querySelector({}).outerHTML", escaped);
                    page.evaluate(script)
                        .await
                        .map_err(|e| Temm1eError::Tool(format!("Failed to get HTML: {}", e)))?
                        .into_value()
                        .map_err(|e| Temm1eError::Tool(format!("Failed to parse HTML: {:?}", e)))?
                } else {
                    page.evaluate("document.documentElement.outerHTML")
                        .await
                        .map_err(|e| Temm1eError::Tool(format!("Failed to get HTML: {}", e)))?
                        .into_value()
                        .map_err(|e| Temm1eError::Tool(format!("Failed to parse HTML: {:?}", e)))?
                };

                // Truncate if too long (safe for multi-byte UTF-8)
                let max_bytes = 15_000;
                let truncated = if html.len() > max_bytes {
                    let boundary = html
                        .char_indices()
                        .map(|(i, _)| i)
                        .take_while(|&i| i <= max_bytes)
                        .last()
                        .unwrap_or(0);
                    format!(
                        "{}...\n\n[Truncated — {} total bytes]",
                        &html[..boundary],
                        html.len()
                    )
                } else {
                    html
                };

                Ok(ToolOutput {
                    content: truncated,
                    is_error: false,
                })
            }

            "save_session" => {
                let session_name = input
                    .arguments
                    .get("session_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");

                let msg = self.save_session(&page, session_name).await?;
                Ok(ToolOutput {
                    content: msg,
                    is_error: false,
                })
            }

            "restore_session" => {
                let session_name = input
                    .arguments
                    .get("session_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");

                let msg = self.restore_session(&page, session_name).await?;
                Ok(ToolOutput {
                    content: msg,
                    is_error: false,
                })
            }

            other => Ok(ToolOutput {
                content: format!(
                    "Unknown action '{}'. Valid actions: navigate, click, click_at, type, screenshot, \
                     get_text, evaluate, get_html, save_session, restore_session, close",
                    other
                ),
                is_error: true,
            }),
        }
    }

    fn take_last_image(&self) -> Option<ToolOutputImage> {
        self.last_image.lock().ok().and_then(|mut img| img.take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Stealth constants tests ──────────────────────────────────────

    #[test]
    fn stealth_user_agent_is_chrome_134() {
        assert!(
            STEALTH_USER_AGENT.contains("Chrome/134"),
            "User-agent should reference Chrome 134, got: {}",
            STEALTH_USER_AGENT
        );
    }

    #[test]
    fn stealth_user_agent_looks_like_windows_desktop() {
        assert!(
            STEALTH_USER_AGENT.contains("Windows NT 10.0"),
            "User-agent should look like a Windows desktop browser"
        );
        assert!(
            STEALTH_USER_AGENT.contains("Win64; x64"),
            "User-agent should indicate 64-bit Windows"
        );
    }

    #[test]
    fn stealth_js_patches_navigator_webdriver() {
        assert!(
            STEALTH_JS.contains("navigator.webdriver") && STEALTH_JS.contains("undefined"),
            "Stealth JS should patch navigator.webdriver to undefined"
        );
    }

    #[test]
    fn stealth_js_patches_navigator_plugins() {
        assert!(
            STEALTH_JS.contains("navigator.plugins"),
            "Stealth JS should patch navigator.plugins"
        );
        assert!(
            STEALTH_JS.contains("Chrome PDF Plugin"),
            "Stealth JS should fake Chrome PDF Plugin"
        );
    }

    #[test]
    fn stealth_js_patches_navigator_languages() {
        assert!(
            STEALTH_JS.contains("navigator.languages"),
            "Stealth JS should patch navigator.languages"
        );
        assert!(
            STEALTH_JS.contains("en-US"),
            "Stealth JS should set en-US as primary language"
        );
    }

    #[test]
    fn stealth_js_patches_chrome_runtime() {
        assert!(
            STEALTH_JS.contains("chrome.runtime"),
            "Stealth JS should hide chrome.runtime"
        );
    }

    #[test]
    fn stealth_js_patches_webgl_fingerprint() {
        assert!(
            STEALTH_JS.contains("WebGLRenderingContext"),
            "Stealth JS should patch WebGL vendor/renderer"
        );
        assert!(
            STEALTH_JS.contains("Intel Inc."),
            "Stealth JS should spoof WebGL vendor as Intel"
        );
        assert!(
            STEALTH_JS.contains("Intel Iris OpenGL Engine"),
            "Stealth JS should spoof WebGL renderer"
        );
    }

    #[test]
    fn stealth_js_patches_webgl2() {
        assert!(
            STEALTH_JS.contains("WebGL2RenderingContext"),
            "Stealth JS should also patch WebGL2 context"
        );
    }

    #[test]
    fn stealth_js_patches_permissions_query() {
        assert!(
            STEALTH_JS.contains("permissions.query"),
            "Stealth JS should patch permissions.query for notifications"
        );
        assert!(
            STEALTH_JS.contains("notifications"),
            "Stealth JS should handle the notifications permission"
        );
    }

    // ── Default idle timeout ─────────────────────────────────────────

    #[test]
    fn default_idle_timeout_is_300_seconds() {
        assert_eq!(DEFAULT_IDLE_TIMEOUT_SECS, 300);
    }

    // ── Session cookie serialization ─────────────────────────────────

    #[test]
    fn session_cookie_serialization_roundtrip() {
        let cookie = SessionCookie {
            name: "session_id".to_string(),
            value: "abc123".to_string(),
            domain: Some(".example.com".to_string()),
            path: Some("/".to_string()),
            expires: Some(1700000000.0),
            http_only: Some(true),
            secure: Some(true),
            same_site: Some("Lax".to_string()),
        };

        let json = serde_json::to_string(&cookie).unwrap();
        let restored: SessionCookie = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.name, "session_id");
        assert_eq!(restored.value, "abc123");
        assert_eq!(restored.domain.as_deref(), Some(".example.com"));
        assert_eq!(restored.path.as_deref(), Some("/"));
        assert_eq!(restored.expires, Some(1700000000.0));
        assert_eq!(restored.http_only, Some(true));
        assert_eq!(restored.secure, Some(true));
        assert_eq!(restored.same_site.as_deref(), Some("Lax"));
    }

    #[test]
    fn session_cookie_with_optional_fields_none() {
        let cookie = SessionCookie {
            name: "minimal".to_string(),
            value: "val".to_string(),
            domain: None,
            path: None,
            expires: None,
            http_only: None,
            secure: None,
            same_site: None,
        };

        let json = serde_json::to_string(&cookie).unwrap();
        let restored: SessionCookie = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.name, "minimal");
        assert_eq!(restored.value, "val");
        assert!(restored.domain.is_none());
        assert!(restored.path.is_none());
        assert!(restored.expires.is_none());
        assert!(restored.http_only.is_none());
        assert!(restored.secure.is_none());
        assert!(restored.same_site.is_none());
    }

    #[test]
    fn session_cookie_vec_serialization() {
        let cookies = vec![
            SessionCookie {
                name: "a".to_string(),
                value: "1".to_string(),
                domain: Some(".foo.com".to_string()),
                path: Some("/".to_string()),
                expires: None,
                http_only: Some(true),
                secure: Some(false),
                same_site: None,
            },
            SessionCookie {
                name: "b".to_string(),
                value: "2".to_string(),
                domain: Some(".bar.com".to_string()),
                path: Some("/api".to_string()),
                expires: Some(9999999999.0),
                http_only: Some(false),
                secure: Some(true),
                same_site: Some("Strict".to_string()),
            },
        ];

        let json = serde_json::to_string_pretty(&cookies).unwrap();
        let restored: Vec<SessionCookie> = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].name, "a");
        assert_eq!(restored[1].name, "b");
        assert_eq!(restored[1].same_site.as_deref(), Some("Strict"));
    }

    #[test]
    fn session_cookie_deserialize_from_json_string() {
        let json = r#"{
            "name": "token",
            "value": "eyJhbGciOiJIUzI1NiJ9",
            "domain": ".github.com",
            "path": "/",
            "expires": 1800000000.0,
            "http_only": true,
            "secure": true,
            "same_site": "None"
        }"#;

        let cookie: SessionCookie = serde_json::from_str(json).unwrap();
        assert_eq!(cookie.name, "token");
        assert_eq!(cookie.domain.as_deref(), Some(".github.com"));
        assert_eq!(cookie.secure, Some(true));
    }

    // ── Session name sanitization ────────────────────────────────────

    #[test]
    fn sanitize_session_name_alphanumeric() {
        assert_eq!(sanitize_session_name("github"), "github");
        assert_eq!(sanitize_session_name("My_Session-01"), "My_Session-01");
    }

    #[test]
    fn sanitize_session_name_dots_allowed() {
        assert_eq!(sanitize_session_name("session.v2"), "session.v2");
    }

    #[test]
    fn sanitize_session_name_replaces_spaces() {
        assert_eq!(sanitize_session_name("my session"), "my_session");
    }

    #[test]
    fn sanitize_session_name_strips_path_traversal() {
        // Dots are kept (allowed chars), slashes replaced with underscores
        assert_eq!(
            sanitize_session_name("../../etc/passwd"),
            ".._.._etc_passwd"
        );
        // Slashes are replaced with underscores so they can't escape the directory
        assert_eq!(sanitize_session_name("/tmp/evil"), "_tmp_evil");
    }

    #[test]
    fn sanitize_session_name_replaces_special_chars() {
        assert_eq!(sanitize_session_name("a@b#c$d"), "a_b_c_d");
    }

    #[test]
    fn sanitize_session_name_empty_becomes_default() {
        assert_eq!(sanitize_session_name(""), "default");
    }

    #[test]
    fn sanitize_session_name_all_special_becomes_default_not() {
        // All chars replaced with underscores, result is not empty
        let result = sanitize_session_name("@#$");
        assert_eq!(result, "___");
    }

    // ── Sessions directory path ──────────────────────────────────────

    #[test]
    fn sessions_dir_returns_correct_path() {
        let dir = sessions_dir().unwrap();
        let path_str = dir.to_string_lossy();
        assert!(
            path_str.ends_with(".temm1e/sessions"),
            "Sessions dir should end with .temm1e/sessions, got: {}",
            path_str
        );
    }

    #[test]
    fn sessions_dir_is_under_home() {
        let dir = sessions_dir().unwrap();
        let home = dirs::home_dir().unwrap();
        assert!(
            dir.starts_with(&home),
            "Sessions dir should be under home directory"
        );
    }

    // ── Timeout configuration ────────────────────────────────────────

    #[test]
    fn browser_tool_default_timeout() {
        // BrowserTool::new() spawns a tokio task, so we need a runtime
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            assert_eq!(tool.idle_timeout_secs, 300);
        });
    }

    #[test]
    fn browser_tool_custom_timeout() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::with_timeout(600);
            assert_eq!(tool.idle_timeout_secs, 600);
        });
    }

    #[test]
    fn browser_tool_short_timeout() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::with_timeout(30);
            assert_eq!(tool.idle_timeout_secs, 30);
        });
    }

    // ── Tool trait tests ─────────────────────────────────────────────

    #[test]
    fn browser_tool_name() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            assert_eq!(tool.name(), "browser");
        });
    }

    #[test]
    fn browser_tool_description_mentions_stealth() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            let desc = tool.description();
            assert!(
                desc.contains("stealth"),
                "Description should mention stealth mode"
            );
        });
    }

    #[test]
    fn browser_tool_description_mentions_session_actions() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            let desc = tool.description();
            assert!(
                desc.contains("save_session"),
                "Description should mention save_session action"
            );
            assert!(
                desc.contains("restore_session"),
                "Description should mention restore_session action"
            );
        });
    }

    #[test]
    fn browser_tool_parameters_schema_includes_session_actions() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            let schema = tool.parameters_schema();
            let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
            let action_strs: Vec<&str> = actions.iter().map(|v| v.as_str().unwrap()).collect();
            assert!(
                action_strs.contains(&"save_session"),
                "Schema should list save_session action"
            );
            assert!(
                action_strs.contains(&"restore_session"),
                "Schema should list restore_session action"
            );
        });
    }

    #[test]
    fn browser_tool_parameters_schema_includes_session_name() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            let schema = tool.parameters_schema();
            assert!(
                schema["properties"]["session_name"].is_object(),
                "Schema should include session_name parameter"
            );
        });
    }

    #[test]
    fn browser_tool_declarations_has_network_access() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            let decl = tool.declarations();
            assert!(!decl.network_access.is_empty());
            assert_eq!(decl.network_access[0], "*");
            assert!(!decl.shell_access);
        });
    }

    // ── Session file I/O tests (using tempdir) ───────────────────────

    #[test]
    fn session_cookie_file_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_session.json");

        let cookies = vec![SessionCookie {
            name: "auth".to_string(),
            value: "secret123".to_string(),
            domain: Some(".example.com".to_string()),
            path: Some("/".to_string()),
            expires: Some(1700000000.0),
            http_only: Some(true),
            secure: Some(true),
            same_site: Some("Lax".to_string()),
        }];

        let json = serde_json::to_string_pretty(&cookies).unwrap();
        std::fs::write(&path, &json).unwrap();

        let read_json = std::fs::read_to_string(&path).unwrap();
        let restored: Vec<SessionCookie> = serde_json::from_str(&read_json).unwrap();

        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].name, "auth");
        assert_eq!(restored[0].value, "secret123");
    }

    #[cfg(unix)]
    #[test]
    fn session_file_permissions_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");

        std::fs::write(&path, "{}").unwrap();
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms).unwrap();

        let metadata = std::fs::metadata(&path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "Session file should be owner-only (0600)");
    }

    // ── SESSIONS_DIR constant ────────────────────────────────────────

    #[test]
    fn sessions_dir_constant_is_sessions() {
        assert_eq!(SESSIONS_DIR, "sessions");
    }

    // ── Additional stealth constants tests ────────────────────────────

    #[test]
    fn stealth_user_agent_contains_safari_string() {
        // A realistic Chrome UA always includes a Safari/ component
        assert!(
            STEALTH_USER_AGENT.contains("Safari/"),
            "User-agent should contain Safari/ for realism"
        );
    }

    #[test]
    fn stealth_user_agent_contains_applewebkit() {
        assert!(
            STEALTH_USER_AGENT.contains("AppleWebKit/"),
            "User-agent should contain AppleWebKit/ for realism"
        );
    }

    #[test]
    fn stealth_user_agent_not_empty() {
        assert!(
            !STEALTH_USER_AGENT.is_empty(),
            "User-agent should not be empty"
        );
        assert!(
            STEALTH_USER_AGENT.len() > 50,
            "User-agent should be a full-length string, got {} chars",
            STEALTH_USER_AGENT.len()
        );
    }

    #[test]
    fn stealth_js_not_empty() {
        assert!(!STEALTH_JS.is_empty(), "Stealth JS should not be empty");
    }

    #[test]
    fn stealth_js_uses_object_defineproperty() {
        // The stealth patches should use Object.defineProperty for robustness
        assert!(
            STEALTH_JS.contains("Object.defineProperty"),
            "Stealth JS should use Object.defineProperty for patches"
        );
    }

    #[test]
    fn stealth_js_patches_webgl_magic_numbers() {
        // UNMASKED_VENDOR_WEBGL = 37445, UNMASKED_RENDERER_WEBGL = 37446
        assert!(
            STEALTH_JS.contains("37445"),
            "Stealth JS should handle UNMASKED_VENDOR_WEBGL (37445)"
        );
        assert!(
            STEALTH_JS.contains("37446"),
            "Stealth JS should handle UNMASKED_RENDERER_WEBGL (37446)"
        );
    }

    #[test]
    fn stealth_js_fakes_three_plugins() {
        // Should fake 3 realistic Chrome plugins
        assert!(
            STEALTH_JS.contains("Chrome PDF Plugin"),
            "Should include Chrome PDF Plugin"
        );
        assert!(
            STEALTH_JS.contains("Chrome PDF Viewer"),
            "Should include Chrome PDF Viewer"
        );
        assert!(
            STEALTH_JS.contains("Native Client"),
            "Should include Native Client"
        );
    }

    // ── Additional session cookie edge cases ─────────────────────────

    #[test]
    fn session_cookie_empty_vec_serialization() {
        let cookies: Vec<SessionCookie> = vec![];
        let json = serde_json::to_string(&cookies).unwrap();
        assert_eq!(json, "[]");
        let restored: Vec<SessionCookie> = serde_json::from_str(&json).unwrap();
        assert!(restored.is_empty());
    }

    #[test]
    fn session_cookie_with_unicode_value() {
        let cookie = SessionCookie {
            name: "lang".to_string(),
            value: "ja-JP".to_string(),
            domain: Some(".example.jp".to_string()),
            path: None,
            expires: None,
            http_only: None,
            secure: None,
            same_site: None,
        };

        let json = serde_json::to_string(&cookie).unwrap();
        let restored: SessionCookie = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.value, "ja-JP");
        assert_eq!(restored.domain.as_deref(), Some(".example.jp"));
    }

    #[test]
    fn session_cookie_with_special_chars_in_value() {
        let cookie = SessionCookie {
            name: "csrf".to_string(),
            value: "a+b/c=d&e%20f".to_string(),
            domain: None,
            path: None,
            expires: None,
            http_only: None,
            secure: None,
            same_site: None,
        };

        let json = serde_json::to_string(&cookie).unwrap();
        let restored: SessionCookie = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.value, "a+b/c=d&e%20f");
    }

    #[test]
    fn session_cookie_large_expiry_value() {
        let cookie = SessionCookie {
            name: "forever".to_string(),
            value: "1".to_string(),
            domain: None,
            path: None,
            expires: Some(f64::MAX),
            http_only: None,
            secure: None,
            same_site: None,
        };

        let json = serde_json::to_string(&cookie).unwrap();
        let restored: SessionCookie = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.expires, Some(f64::MAX));
    }

    #[test]
    fn session_cookie_zero_expiry() {
        // expires = 0 means session-only cookie
        let cookie = SessionCookie {
            name: "temp".to_string(),
            value: "x".to_string(),
            domain: None,
            path: None,
            expires: Some(0.0),
            http_only: None,
            secure: None,
            same_site: None,
        };

        let json = serde_json::to_string(&cookie).unwrap();
        let restored: SessionCookie = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.expires, Some(0.0));
    }

    // ── Additional session name sanitization edge cases ──────────────

    #[test]
    fn sanitize_session_name_unicode() {
        // Unicode chars are not alphanumeric in Rust's is_alphanumeric(),
        // but CJK/cyrillic chars actually ARE alphanumeric. Test both.
        let result = sanitize_session_name("session_name");
        assert_eq!(result, "session_name");
    }

    #[test]
    fn sanitize_session_name_backslash() {
        // Windows-style path traversal: dots kept, backslashes replaced
        assert_eq!(sanitize_session_name("..\\..\\evil"), ".._.._evil");
    }

    #[test]
    fn sanitize_session_name_very_long() {
        let long_name = "a".repeat(1000);
        let result = sanitize_session_name(&long_name);
        assert_eq!(result.len(), 1000);
        assert_eq!(result, long_name);
    }

    #[test]
    fn sanitize_session_name_dashes_and_underscores_mixed() {
        assert_eq!(sanitize_session_name("my-session_v2.0"), "my-session_v2.0");
    }

    #[test]
    fn sanitize_session_name_leading_dot() {
        // Leading dots are allowed (they're valid chars)
        assert_eq!(sanitize_session_name(".hidden"), ".hidden");
    }

    // ── Timeout edge cases ───────────────────────────────────────────

    #[test]
    fn browser_tool_zero_timeout() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::with_timeout(0);
            assert_eq!(tool.idle_timeout_secs, 0);
        });
    }

    #[test]
    fn browser_tool_large_timeout() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::with_timeout(86400); // 24 hours
            assert_eq!(tool.idle_timeout_secs, 86400);
        });
    }

    #[test]
    fn browser_tool_default_impl() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // BrowserTool::default() should delegate to new()
            let tool = BrowserTool::default();
            assert_eq!(tool.idle_timeout_secs, 300);
        });
    }

    // ── Session file I/O edge cases ──────────────────────────────────

    #[test]
    fn session_cookie_file_not_found_gives_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent_session.json");
        let result = std::fs::read_to_string(&path);
        assert!(result.is_err(), "Reading nonexistent file should fail");
    }

    #[test]
    fn session_cookie_file_invalid_json_gives_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_session.json");
        std::fs::write(&path, "not valid json {{{").unwrap();

        let json = std::fs::read_to_string(&path).unwrap();
        let result: Result<Vec<SessionCookie>, _> = serde_json::from_str(&json);
        assert!(
            result.is_err(),
            "Parsing invalid JSON should return an error"
        );
    }

    #[test]
    fn session_cookie_file_empty_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty_session.json");
        std::fs::write(&path, "[]").unwrap();

        let json = std::fs::read_to_string(&path).unwrap();
        let cookies: Vec<SessionCookie> = serde_json::from_str(&json).unwrap();
        assert!(cookies.is_empty());
    }

    #[test]
    fn session_cookie_file_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        let path = nested.join("session.json");
        std::fs::write(&path, "[]").unwrap();
        assert!(path.exists());
    }

    // ── Tool schema completeness ─────────────────────────────────────

    #[test]
    fn browser_tool_schema_lists_all_ten_actions() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            let schema = tool.parameters_schema();
            let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
            let action_strs: Vec<&str> = actions.iter().map(|v| v.as_str().unwrap()).collect();

            let expected = vec![
                "navigate",
                "click",
                "click_at",
                "type",
                "screenshot",
                "get_text",
                "evaluate",
                "get_html",
                "save_session",
                "restore_session",
                "close",
            ];

            assert_eq!(
                action_strs.len(),
                expected.len(),
                "Should have exactly {} actions, got: {:?}",
                expected.len(),
                action_strs
            );
            for action in &expected {
                assert!(action_strs.contains(action), "Missing action: {}", action);
            }
        });
    }

    #[test]
    fn browser_tool_schema_action_is_required() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            let schema = tool.parameters_schema();
            let required = schema["required"].as_array().unwrap();
            let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
            assert!(
                required_strs.contains(&"action"),
                "action should be required"
            );
        });
    }

    #[test]
    fn browser_tool_close_when_not_running() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            // Close when no browser is running should not panic
            let msg = tool.close_browser().await;
            assert_eq!(msg, "No browser was running.");
        });
    }
}
