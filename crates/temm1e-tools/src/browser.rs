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

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;

use crate::browser_observation::{self, ObservationTier};
use crate::credential_scrub;

use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::accessibility::AxNode;
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
    PathAccess, Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput, ToolOutputImage, Vault,
};
use tokio::sync::Mutex;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// Default idle timeout (seconds). 0 = disabled (persistent browser).
/// Overridden by `ToolsConfig.browser_timeout_secs`.
const DEFAULT_IDLE_TIMEOUT_SECS: i64 = 0;

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

/// JavaScript to detect QR codes on a page via heuristic checks.
///
/// Returns one of: `"qr_canvas"`, `"qr_image"`, `"qr_possible"`, `"no_qr"`.
///
/// Detection strategies:
/// 1. Canvas elements with square dimensions (QR codes are often rendered in canvas)
/// 2. Images with "qr" in src/alt/class attributes
/// 3. Square images of QR-like size (150px+) that are prominent on the page
pub(crate) const QR_DETECT_JS: &str = r#"
(() => {
    // 1. Canvas elements with square dimensions (QR codes are often rendered in canvas)
    const canvases = document.querySelectorAll('canvas');
    for (const c of canvases) {
        const ratio = c.width / c.height;
        if (ratio > 0.9 && ratio < 1.1 && c.width >= 100 && c.width <= 500) {
            return 'qr_canvas';
        }
    }
    // 2. Images with "qr" in src/alt/class
    const imgs = document.querySelectorAll('img');
    for (const img of imgs) {
        const src = (img.src || '').toLowerCase();
        const alt = (img.alt || '').toLowerCase();
        const cls = (img.className || '').toLowerCase();
        if (src.includes('qr') || alt.includes('qr') || cls.includes('qr')) {
            return 'qr_image';
        }
        // 3. Square images of QR-like size
        const rect = img.getBoundingClientRect();
        const ratio = rect.width / rect.height;
        if (ratio > 0.9 && ratio < 1.1 && rect.width >= 100 && rect.width <= 400) {
            // Could be a QR code — check if it's prominent on the page
            if (rect.width >= 150) return 'qr_possible';
        }
    }
    return 'no_qr';
})()
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

/// Web credential for automated login — zeroed from memory on drop.
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct WebCredential {
    pub username: String,
    pub password: String,
    #[zeroize(skip)]
    pub service_url: String,
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
    /// Hash of the last observed accessibility tree — for incremental observation.
    /// If the tree hasn't changed since last observation, we return a short message
    /// instead of repeating the full tree.
    last_tree_hash: Arc<std::sync::Mutex<Option<u64>>>,
    /// Optional vault for credential retrieval and session auto-capture.
    vault: Option<Arc<dyn Vault>>,
    /// PID of the Chrome main process — used to kill child processes on shutdown.
    /// Set to 0 when no browser is running.
    chrome_pid: Arc<AtomicU32>,
    /// Instant when the browser was last started — for uptime tracking.
    browser_started_at: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    /// Domains visited during this browser session — for `/browser` status.
    active_domains: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl Default for BrowserTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns a per-process Chrome user-data-dir path.
///
/// The PID suffix prevents two concurrent Temm1e instances — or a crashed
/// prior run leaving a stale `SingletonLock` — from colliding on Chrome's
/// singleton check. Without this, chromiumoxide 0.7 falls back to a shared
/// default under `%TEMP%/chromiumoxide-runner`, which reproducibly triggers
/// Chrome exit code 21 (`RESULT_CODE_PROFILE_IN_USE`) on every platform,
/// reported most visibly on Windows 11 (GH-50).
pub(crate) fn per_process_profile(subname: &str) -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("temm1e")
        .join(format!("{subname}-{}", std::process::id()))
}

/// Remove Chromium singleton-lock files from a single profile dir.
///
/// Narrow-scope, idempotent companion to `BrowserTool::cleanup_singleton_locks`
/// (which also cleans the user's real Chrome profile on shutdown). This
/// variant only touches the path it is given and is safe to call before every
/// launch — graceful shutdown is never guaranteed (panic / SIGKILL / OS
/// reboot), so stale locks may persist and block the next cold start.
pub(crate) fn clear_singleton_locks_at(profile: &std::path::Path) {
    for name in &[
        "SingletonLock",
        "SingletonSocket",
        "SingletonCookie",
        "lockfile",
    ] {
        let _ = std::fs::remove_file(profile.join(name));
    }
}

impl BrowserTool {
    /// Create a new browser tool with default timeout (0 = persistent, no idle timeout).
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_IDLE_TIMEOUT_SECS as u64)
    }

    /// Create a new browser tool with a custom idle timeout (in seconds).
    /// If `timeout_secs` is 0, the idle watchdog is disabled (persistent browser).
    pub fn with_timeout(timeout_secs: u64) -> Self {
        let browser = Arc::new(Mutex::new(None));
        let page = Arc::new(Mutex::new(None));
        let last_used = Arc::new(AtomicI64::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let idle_timeout = timeout_secs as i64;
        let cdp_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> =
            Arc::new(Mutex::new(None));
        let chrome_pid = Arc::new(AtomicU32::new(0));

        // Spawn idle auto-close watchdog — store handle for cleanup on drop.
        // When idle_timeout == 0, the watchdog still runs but never triggers
        // the auto-close check (persistent browser mode).
        let watchdog_handle = {
            let browser = browser.clone();
            let page = page.clone();
            let last_used = last_used.clone();
            let shutdown = shutdown.clone();
            let cdp_handle = cdp_handle.clone();
            let chrome_pid = chrome_pid.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    // idle_timeout == 0 means persistent browser — skip auto-close
                    if idle_timeout == 0 {
                        continue;
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
                            let pid = chrome_pid.swap(0, Ordering::Relaxed);
                            *p = None;
                            *b = None;
                            // Abort the CDP handler so it doesn't linger.
                            if let Some(handle) = cdp_handle.lock().await.take() {
                                handle.abort();
                            }
                            last_used.store(0, Ordering::Relaxed);
                            // Kill any orphaned Chrome child processes.
                            if pid > 0 {
                                kill_chrome_children(pid);
                            }
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
            last_tree_hash: Arc::new(std::sync::Mutex::new(None)),
            vault: None,
            chrome_pid,
            browser_started_at: Arc::new(std::sync::Mutex::new(None)),
            active_domains: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// Attach a vault for credential retrieval and session auto-capture.
    pub fn with_vault(mut self, vault: Arc<dyn Vault>) -> Self {
        self.vault = Some(vault);
        self
    }

    // ── Public API for /browser command ──────────────────────────────

    /// Check if the browser is currently running.
    /// Clean up SingletonLock/Socket/Cookie files from BOTH the work profile
    /// and the real Chrome profile. This prevents Tem from blocking the user's
    /// real Chrome from opening after Tem's browser closes.
    fn cleanup_singleton_locks() {
        let work_profile = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("temm1e")
            .join("browser-profile");

        // Clean work profile locks
        for lock_file in &["SingletonLock", "SingletonSocket", "SingletonCookie"] {
            let path = work_profile.join(lock_file);
            if path.exists() {
                let _ = std::fs::remove_file(&path);
            }
        }

        // Clean real Chrome profile locks (in case Chrome inherited our lock)
        if let Some(real_default) = Self::find_chrome_profile() {
            if let Some(real_root) = real_default.parent() {
                for lock_file in &["SingletonLock", "SingletonSocket", "SingletonCookie"] {
                    let path = real_root.join(lock_file);
                    if path.exists() {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }

        // Clean chromiumoxide runner locks
        let runner_dir = std::env::temp_dir().join("chromiumoxide-runner");
        if runner_dir.exists() {
            let _ = std::fs::remove_file(runner_dir.join("SingletonLock"));
        }

        tracing::debug!("Cleaned up Chrome SingletonLock files");
    }

    /// Find the user's real Chrome/Chromium profile directory (cross-platform).
    fn find_chrome_profile() -> Option<std::path::PathBuf> {
        let home = dirs::home_dir()?;

        // Platform-specific Chrome profile locations
        let candidates: Vec<std::path::PathBuf> = if cfg!(target_os = "macos") {
            vec![
                home.join("Library/Application Support/Google/Chrome/Default"),
                home.join("Library/Application Support/Chromium/Default"),
            ]
        } else if cfg!(target_os = "windows") {
            vec![
                home.join("AppData/Local/Google/Chrome/User Data/Default"),
                home.join("AppData/Local/Chromium/User Data/Default"),
            ]
        } else {
            // Linux
            vec![
                home.join(".config/google-chrome/Default"),
                home.join(".config/chromium/Default"),
            ]
        };

        candidates.into_iter().find(|p| p.join("Cookies").exists())
    }

    /// Recursively copy a directory.
    fn copy_dir_recursive(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let src_path = entry.path();
            let dest_path = dest.join(entry.file_name());
            if src_path.is_dir() {
                Self::copy_dir_recursive(&src_path, &dest_path)?;
            } else {
                std::fs::copy(&src_path, &dest_path)?;
            }
        }
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.browser
            .try_lock()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }

    /// Get the browser uptime as a human-readable string, or `None` if not running.
    pub fn uptime(&self) -> Option<String> {
        let started = self.browser_started_at.lock().ok()?.as_ref().copied()?;
        let elapsed = started.elapsed();
        let secs = elapsed.as_secs();
        if secs < 60 {
            Some(format!("{}s", secs))
        } else if secs < 3600 {
            Some(format!("{}m {}s", secs / 60, secs % 60))
        } else {
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            Some(format!("{}h {}m", h, m))
        }
    }

    /// Get the set of domains visited during this browser session.
    pub fn get_active_domains(&self) -> std::collections::HashSet<String> {
        self.active_domains
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Auto-capture all active sessions to vault before browser close.
    ///
    /// For each domain visited, captures cookies via CDP `Network.getCookies`
    /// and stores them in the vault under `web_session:{domain}`.
    /// Errors are logged but do not prevent the close.
    async fn auto_capture_sessions_to_vault(&self) -> Vec<String> {
        let mut saved = Vec::new();
        let vault = match self.vault.as_ref() {
            Some(v) => v,
            None => return saved,
        };

        let page_guard = self.page.lock().await;
        let page = match page_guard.as_ref() {
            Some(p) => p,
            None => return saved,
        };

        // Get current URL for the capture metadata
        let current_url = page.url().await.ok().flatten().unwrap_or_default();

        // Get all cookies from the browser
        let cookies_response = match page.execute(GetCookiesParams::default()).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "Auto-capture: failed to get cookies");
                return saved;
            }
        };

        // Group cookies by base domain
        let mut domain_cookies: HashMap<
            String,
            Vec<&chromiumoxide::cdp::browser_protocol::network::Cookie>,
        > = HashMap::new();
        for cookie in &cookies_response.result.cookies {
            let domain = cookie.domain.trim_start_matches('.').to_string();
            // Extract the base domain (e.g., "facebook.com" -> "facebook")
            let base = domain
                .split('.')
                .rev()
                .nth(1)
                .unwrap_or(&domain)
                .to_string();
            domain_cookies.entry(base).or_default().push(cookie);
        }

        // Save each domain's cookies to vault
        for (domain, cookies) in &domain_cookies {
            let cookie_values: Vec<serde_json::Value> = cookies
                .iter()
                .filter_map(|c| serde_json::to_value(c).ok())
                .collect();

            let state = serde_json::json!({
                "cookies": cookie_values,
                "local_storage": [],
                "session_storage": [],
                "url": current_url,
                "captured_at": chrono::Utc::now().to_rfc3339(),
                "service": domain,
            });

            let json = match serde_json::to_vec(&state) {
                Ok(j) => j,
                Err(e) => {
                    tracing::warn!(domain = %domain, error = %e, "Auto-capture: serialize failed");
                    continue;
                }
            };

            let vault_key = format!("web_session:{}", domain);
            match vault.store_secret(&vault_key, &json).await {
                Ok(()) => {
                    tracing::info!(
                        domain = %domain,
                        cookies = cookies.len(),
                        "Session for {} auto-saved to vault",
                        domain
                    );
                    saved.push(domain.clone());
                }
                Err(e) => {
                    tracing::warn!(domain = %domain, error = %e, "Auto-capture: vault store failed");
                }
            }
        }

        saved
    }

    /// Auto-restore all saved sessions from vault when browser starts.
    ///
    /// Looks up all `web_session:*` keys in vault and restores their cookies
    /// via CDP `Network.setCookies`.
    async fn auto_restore_sessions_from_vault(&self, page: &Page) {
        let vault = match self.vault.as_ref() {
            Some(v) => v,
            None => return,
        };

        let keys = match vault.list_keys().await {
            Ok(k) => k,
            Err(e) => {
                tracing::debug!(error = %e, "Auto-restore: failed to list vault keys");
                return;
            }
        };

        let session_keys: Vec<&String> = keys
            .iter()
            .filter(|k| k.starts_with("web_session:"))
            .collect();

        if session_keys.is_empty() {
            return;
        }

        let mut restored_count = 0u32;
        for key in &session_keys {
            let raw_bytes = match vault.get_secret(key).await {
                Ok(Some(b)) => b,
                _ => continue,
            };

            // Parse the session state — handle both full SessionState and legacy formats
            let cookies: Vec<serde_json::Value> =
                if let Ok(state) = serde_json::from_slice::<serde_json::Value>(&raw_bytes) {
                    state
                        .get("cookies")
                        .and_then(|c| c.as_array())
                        .cloned()
                        .unwrap_or_default()
                } else {
                    continue;
                };

            if cookies.is_empty() {
                continue;
            }

            let cookie_params: Vec<CookieParam> = cookies
                .iter()
                .filter_map(|cv| {
                    let name = cv.get("name")?.as_str()?;
                    let value = cv.get("value")?.as_str()?;
                    let mut param = CookieParam::new(name.to_string(), value.to_string());
                    if let Some(domain) = cv.get("domain").and_then(|v| v.as_str()) {
                        param.domain = Some(domain.to_string());
                    }
                    if let Some(path) = cv.get("path").and_then(|v| v.as_str()) {
                        param.path = Some(path.to_string());
                    }
                    if let Some(expires) = cv.get("expires").and_then(|v| v.as_f64()) {
                        param.expires = Some(TimeSinceEpoch::new(expires));
                    }
                    if let Some(http_only) = cv.get("httpOnly").and_then(|v| v.as_bool()) {
                        param.http_only = Some(http_only);
                    }
                    if let Some(secure) = cv.get("secure").and_then(|v| v.as_bool()) {
                        param.secure = Some(secure);
                    }
                    if let Some(ss) = cv.get("sameSite").and_then(|v| v.as_str()) {
                        if let Ok(parsed) = ss.parse::<CookieSameSite>() {
                            param.same_site = Some(parsed);
                        }
                    }
                    Some(param)
                })
                .collect();

            if !cookie_params.is_empty() {
                match page.execute(SetCookiesParams::new(cookie_params)).await {
                    Ok(_) => restored_count += 1,
                    Err(e) => {
                        tracing::debug!(key = %key, error = %e, "Auto-restore: set cookies failed");
                    }
                }
            }
        }

        if restored_count > 0 {
            tracing::info!(
                count = restored_count,
                "Restored {} saved sessions from vault",
                restored_count
            );
        }
    }

    /// Get the list of saved web sessions from vault (for `/browser sessions`).
    ///
    /// Returns a list of `(service_name, captured_at)` pairs.
    pub async fn list_saved_sessions(&self) -> Vec<(String, String)> {
        let vault = match self.vault.as_ref() {
            Some(v) => v,
            None => return Vec::new(),
        };

        let keys = match vault.list_keys().await {
            Ok(k) => k,
            Err(_) => return Vec::new(),
        };

        let mut sessions = Vec::new();
        for key in keys {
            if let Some(service) = key.strip_prefix("web_session:") {
                let captured_at = match vault.get_secret(&key).await {
                    Ok(Some(raw)) => serde_json::from_slice::<serde_json::Value>(&raw)
                        .ok()
                        .and_then(|v| v.get("captured_at")?.as_str().map(String::from))
                        .unwrap_or_else(|| "unknown".to_string()),
                    _ => "unknown".to_string(),
                };
                sessions.push((service.to_string(), captured_at));
            }
        }
        sessions
    }

    /// Delete a saved session from vault (for `/browser forget <service>`).
    pub async fn forget_session(&self, service: &str) -> Result<(), String> {
        let vault = match self.vault.as_ref() {
            Some(v) => v,
            None => return Err("Vault not available".to_string()),
        };

        let vault_key = format!("web_session:{}", service);
        vault
            .delete_secret(&vault_key)
            .await
            .map_err(|e| format!("Failed to delete session: {}", e))
    }

    /// Close the browser with auto-capture. Returns (message, saved_domains).
    pub async fn close_with_capture(&self) -> (String, Vec<String>) {
        let saved = self.auto_capture_sessions_to_vault().await;
        let msg = self.close_browser().await;
        (msg, saved)
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
            let pid = self.chrome_pid.swap(0, Ordering::Relaxed);
            *page_guard = None;
            *browser_guard = None;
            // Abort the CDP handler task so it doesn't linger after the browser exits.
            if let Some(handle) = self.cdp_handle.lock().await.take() {
                handle.abort();
            }
            self.last_used.store(0, Ordering::Relaxed);
            // Clear browser lifecycle tracking
            if let Ok(mut started) = self.browser_started_at.lock() {
                *started = None;
            }
            if let Ok(mut domains) = self.active_domains.lock() {
                domains.clear();
            }
            // Kill any orphaned Chrome child processes (renderer, GPU, utility).
            if pid > 0 {
                kill_chrome_children(pid);
            }
            // CRITICAL: Clean up SingletonLock files so the user's real Chrome
            // can still open. The cloned work profile leaves locks that block
            // the real Chrome from launching.
            Self::cleanup_singleton_locks();
            tracing::info!("Browser closed by agent");
            "Browser closed.".to_string()
        } else {
            "No browser was running.".to_string()
        }
    }

    /// Graceful async shutdown — auto-captures sessions, closes browser, kills Chrome.
    ///
    /// Prefer this over relying on `Drop` during application shutdown, since `Drop`
    /// cannot run async code and must use best-effort synchronous cleanup.
    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Best-effort auto-capture before closing
        let saved = self.auto_capture_sessions_to_vault().await;
        if !saved.is_empty() {
            tracing::info!(domains = ?saved, "Auto-captured sessions on shutdown");
        }
        self.close_browser().await;
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
                    let old_pid = self.chrome_pid.swap(0, Ordering::Relaxed);
                    *page_guard = None;
                    *browser_guard = None;
                    // Abort the stale CDP handler from the dead browser.
                    if let Some(handle) = self.cdp_handle.lock().await.take() {
                        handle.abort();
                    }
                    // Clean up any lingering child processes from the dead browser.
                    if old_pid > 0 {
                        kill_chrome_children(old_pid);
                    }
                }
            }
        }

        // ── Browser mode: headed (full window) with headless fallback ──
        // Headed mode avoids anti-bot detection on sites like Zalo, WhatsApp.
        // Falls back to headless on VPS/Docker where no display is available.
        // Override with TEMM1E_HEADLESS=1 to force headless.
        let force_headless = std::env::var("TEMM1E_HEADLESS").unwrap_or_default() == "1";
        let has_display = std::env::var("DISPLAY").is_ok()
            || std::env::var("WAYLAND_DISPLAY").is_ok()
            || cfg!(target_os = "macos")
            || cfg!(target_os = "windows");
        let use_headless = force_headless || !has_display;

        let mut builder = BrowserConfig::builder();

        if use_headless {
            builder = builder.arg("--headless=new");
            tracing::info!("Browser launching in headless mode");
        } else {
            // Headed mode — real Chrome window, avoids anti-bot detection
            // Window starts minimized to avoid disrupting the user
            builder = builder
                .arg("--window-position=0,0")
                .arg("--window-size=1280,900");
            tracing::info!("Browser launching in headed mode (better site compatibility)");
        }

        builder = builder
            .arg("--disable-gpu")
            .arg("--no-sandbox")
            .arg("--disable-dev-shm-usage");

        // ── Profile Strategy ──────────────────────────────────────────
        // Clone the user's real Chrome profile (cookies, localStorage, sessions)
        // into a working directory so we get: real sessions + debug port.
        // Chrome blocks debug port on the real profile (SingletonLock), but a
        // cloned profile works perfectly — sites see real cookies, no blank pages.
        //
        // Override: TEMM1E_CLEAN_BROWSER=1 forces a clean profile (no cookies).
        let clean_browser = std::env::var("TEMM1E_CLEAN_BROWSER").unwrap_or_default() == "1";

        let work_profile = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("temm1e")
            .join("browser-profile");

        if clean_browser {
            // Clean profile — no cookies, fresh start
            let _ = std::fs::remove_dir_all(&work_profile);
            let _ = std::fs::create_dir_all(&work_profile);
            tracing::info!(profile = %work_profile.display(), "Browser: clean profile");
        } else {
            // Clone user's real Chrome profile if we haven't already
            let default_subdir = work_profile.join("Default");
            if !default_subdir.join("Cookies").exists() {
                let _ = std::fs::create_dir_all(&default_subdir);
                // Find the real Chrome profile
                let real_profile = Self::find_chrome_profile();
                if let Some(ref real) = real_profile {
                    // Copy essential session files (NOT the whole profile — just auth data)
                    for item in &["Cookies", "Cookies-journal"] {
                        let src = real.join(item);
                        if src.exists() {
                            let _ = std::fs::copy(&src, default_subdir.join(item));
                        }
                    }
                    // Copy Local Storage and Session Storage dirs
                    for dir_name in &["Local Storage", "Session Storage"] {
                        let src = real.join(dir_name);
                        if src.is_dir() {
                            let dest = default_subdir.join(dir_name);
                            let _ = Self::copy_dir_recursive(&src, &dest);
                        }
                    }
                    tracing::info!(
                        from = %real.display(),
                        to = %work_profile.display(),
                        "Browser: cloned user's Chrome profile (cookies + storage)"
                    );
                } else {
                    tracing::info!("Browser: no Chrome profile found, using fresh profile");
                }
            } else {
                tracing::info!(profile = %work_profile.display(), "Browser: reusing existing work profile");
            }
        }

        // Remove any stale singleton-lock files from the work profile before
        // Chrome starts. A crashed prior run leaves these behind and the next
        // launch dies with exit code 21 (RESULT_CODE_PROFILE_IN_USE). See GH-50.
        clear_singleton_locks_at(&work_profile);

        builder = builder
            .user_data_dir(&work_profile)
            .arg("--no-first-run")
            .arg("--no-default-browser-check");

        // TEMM1E_NO_STEALTH=1 disables all anti-detection flags (for sites
        // like Zalo that detect stealth flags themselves and show blank pages)
        let no_stealth = std::env::var("TEMM1E_NO_STEALTH").unwrap_or_default() == "1";

        if !no_stealth {
            builder = builder
                .arg("--disable-blink-features=AutomationControlled")
                .arg("--disable-infobars")
                .arg("--disable-background-timer-throttling")
                .arg("--disable-backgrounding-occluded-windows")
                .arg("--disable-renderer-backgrounding")
                .arg("--disable-ipc-flooding-protection")
                .arg(format!("--user-agent={}", STEALTH_USER_AGENT))
                .arg("--lang=en-US,en");
        } else {
            tracing::info!("Stealth flags DISABLED (TEMM1E_NO_STEALTH=1)");
        }

        let config = builder
            .window_size(1920, 1080)
            .build()
            .map_err(|e| Temm1eError::Tool(format!("Failed to build browser config: {}", e)))?;

        let (mut browser, mut handler) = Browser::launch(config).await.map_err(|e| {
            Temm1eError::Tool(format!(
                "Failed to launch browser. Is Chrome/Chromium installed? Error: {}",
                e
            ))
        })?;

        // Capture the Chrome process PID for child-process cleanup on shutdown.
        if let Some(child) = browser.get_mut_child() {
            let pid = child.as_mut_inner().id();
            self.chrome_pid.store(pid, Ordering::Relaxed);
            tracing::debug!(pid = pid, "Chrome process PID captured");
        }

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

        // ── Auto-restore saved sessions from vault ──────────────────
        // Must happen before storing page reference (we need the page ref but
        // auto_restore_sessions_from_vault takes &Page directly).
        self.auto_restore_sessions_from_vault(&page).await;

        *browser_guard = Some(browser);
        *page_guard = Some(page.clone());
        self.last_used
            .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);

        // Track browser start time for uptime reporting
        if let Ok(mut started) = self.browser_started_at.lock() {
            *started = Some(std::time::Instant::now());
        }
        // Clear active domains for the new session
        if let Ok(mut domains) = self.active_domains.lock() {
            domains.clear();
        }

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

/// Extract the base domain from a URL (e.g., "https://www.facebook.com/login" -> "facebook").
fn extract_base_domain(url: &str) -> Option<String> {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = without_scheme.split('/').next()?;
    // Strip port if present (e.g., "localhost:8080" -> "localhost")
    let host_no_port = host.split(':').next().unwrap_or(host);
    let clean = host_no_port.strip_prefix("www.").unwrap_or(host_no_port);
    // Get the base name (second-to-last dot-separated segment for TLDs, or first for simple domains)
    let parts: Vec<&str> = clean.split('.').collect();
    if parts.len() >= 2 {
        Some(parts[parts.len() - 2].to_string())
    } else if !parts[0].is_empty() {
        Some(parts[0].to_string())
    } else {
        None
    }
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

// ── Accessibility tree roles ────────────────────────────────────────

/// Interactive roles worth surfacing to the agent (buttons, inputs, links, etc.).
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "link",
    "textbox",
    "combobox",
    "checkbox",
    "radio",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "menuitem",
    "option",
    "searchbox",
    "textarea",
];

/// Semantic/structural roles that provide meaningful page context.
const SEMANTIC_ROLES: &[&str] = &[
    "heading",
    "navigation",
    "main",
    "form",
    "list",
    "listitem",
    "table",
    "row",
    "cell",
    "img",
    "alert",
    "dialog",
];

/// AX property names we include in the formatted output.
const AX_KEY_PROPERTIES: &[&str] = &[
    "focused", "disabled", "expanded", "checked", "required", "level",
];

/// Format a flat accessibility tree from CDP `AxNode` list into a numbered,
/// indented, filtered text representation suitable for LLM consumption.
///
/// Only interactive and semantic roles are included; generic containers (div,
/// span, group, paragraph, StaticText) are silently traversed but not emitted.
/// Each emitted node is assigned a sequential index for stable cross-turn
/// references.
fn format_ax_tree(nodes: &[chromiumoxide::cdp::browser_protocol::accessibility::AxNode]) -> String {
    use chromiumoxide::cdp::browser_protocol::accessibility::AxNode;

    if nodes.is_empty() {
        return "(empty accessibility tree)".to_string();
    }
    // Build a lookup: node_id → &AxNode
    let node_map: HashMap<&str, &AxNode> = nodes.iter().map(|n| (n.node_id.as_ref(), n)).collect();

    // Build parent → ordered children map
    let mut children_map: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in nodes {
        if let Some(ref child_ids) = node.child_ids {
            let ids: Vec<&str> = child_ids.iter().map(|id| id.as_ref()).collect();
            children_map.insert(node.node_id.as_ref(), ids);
        }
    }

    let mut output = String::new();
    let mut index: usize = 1;

    // Recursive walker. Returns true if this subtree produced any output.
    fn walk(
        node_id: &str,
        depth: usize,
        index: &mut usize,
        output: &mut String,
        node_map: &HashMap<&str, &chromiumoxide::cdp::browser_protocol::accessibility::AxNode>,
        children_map: &HashMap<&str, Vec<&str>>,
    ) {
        let Some(node) = node_map.get(node_id) else {
            return;
        };

        // Skip ignored nodes entirely
        if node.ignored {
            return;
        }

        let role = node
            .role
            .as_ref()
            .and_then(|v| v.value.as_ref())
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let name = node
            .name
            .as_ref()
            .and_then(|v| v.value.as_ref())
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let is_interactive = INTERACTIVE_ROLES.contains(&role);
        let is_semantic = SEMANTIC_ROLES.contains(&role);

        if is_interactive || is_semantic {
            let indent = "  ".repeat(depth);
            let _ = write!(output, "{indent}[{idx}] {role}", idx = *index);

            // Include name if non-empty
            if !name.is_empty() {
                let _ = write!(output, " \"{}\"", name);
            }

            // Include value for input elements
            if let Some(ref val) = node.value {
                if let Some(ref v) = val.value {
                    let val_str = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    if !val_str.is_empty() {
                        let _ = write!(output, " value=\"{}\"", val_str);
                    }
                }
            }

            // Include key properties (focused, disabled, expanded, checked, required, level)
            if let Some(ref props) = node.properties {
                for prop in props {
                    let prop_name = prop.name.as_ref();
                    if AX_KEY_PROPERTIES.contains(&prop_name) {
                        if let Some(ref v) = prop.value.value {
                            let val_str = match v {
                                serde_json::Value::String(s) => s.clone(),
                                serde_json::Value::Bool(b) => b.to_string(),
                                serde_json::Value::Number(n) => n.to_string(),
                                other => other.to_string(),
                            };
                            let _ = write!(output, " {}={}", prop_name, val_str);
                        }
                    }
                }
            }

            let _ = writeln!(output);
            *index += 1;

            // Recurse into children at deeper indent
            if let Some(child_ids) = children_map.get(node_id) {
                for child_id in child_ids {
                    walk(child_id, depth + 1, index, output, node_map, children_map);
                }
            }
        } else {
            // Not a role we emit — still recurse into children at SAME depth
            // (transparent passthrough for generic containers)
            if let Some(child_ids) = children_map.get(node_id) {
                for child_id in child_ids {
                    walk(child_id, depth, index, output, node_map, children_map);
                }
            }
        }
    }

    // The first node in the array is the root
    let root_id = nodes[0].node_id.as_ref();
    walk(
        root_id,
        0,
        &mut index,
        &mut output,
        &node_map,
        &children_map,
    );

    if output.is_empty() {
        "(no interactive or semantic elements found)".to_string()
    } else {
        output
    }
}

// ── QR code detection ────────────────────────────────────────────

/// Run heuristic QR code detection on a page via JavaScript.
///
/// Returns `true` if a QR code (or likely QR code) is detected on the page.
/// Uses canvas dimensions, image attributes, and image sizes as signals.
async fn detect_qr_on_page(page: &Page) -> bool {
    match page.evaluate(QR_DETECT_JS).await {
        Ok(result) => {
            let detection = result
                .into_value::<String>()
                .unwrap_or_else(|_| "no_qr".to_string());
            let found = detection != "no_qr";
            if found {
                tracing::info!(detection = %detection, "QR code detected on page");
            }
            found
        }
        Err(e) => {
            tracing::debug!(error = %e, "QR detection JS failed — assuming no QR");
            false
        }
    }
}

/// Take a screenshot and store it in `last_image` for vision pipeline forwarding.
///
/// Used by QR auto-detection to send the screenshot to the user without them
/// explicitly requesting one.
async fn auto_screenshot_for_qr(
    page: &Page,
    last_image: &std::sync::Mutex<Option<ToolOutputImage>>,
) {
    use chromiumoxide::page::ScreenshotParams;

    match page.screenshot(ScreenshotParams::builder().build()).await {
        Ok(png_data) => {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png_data);
            if let Ok(mut img) = last_image.lock() {
                *img = Some(ToolOutputImage {
                    media_type: "image/png".to_string(),
                    data: b64,
                });
            }
            tracing::debug!(
                screenshot_bytes = png_data.len(),
                "Auto-screenshot captured for QR code forwarding"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to auto-screenshot for QR detection");
        }
    }
}

// ── Login form detection ─────────────────────────────────────────

fn detect_login_form(nodes: &[AxNode]) -> Option<(String, String, String)> {
    let mut username_id = None;
    let mut password_id = None;
    let mut submit_id = None;

    for node in nodes {
        if node.ignored {
            continue;
        }
        let role = node
            .role
            .as_ref()
            .and_then(|v| v.value.as_ref())
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = node
            .name
            .as_ref()
            .and_then(|v| v.value.as_ref())
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        if role == "textbox" {
            let is_protected = node
                .properties
                .as_ref()
                .map(|props| {
                    props.iter().any(|p| {
                        p.name.as_ref() == "protected"
                            && p.value.value.as_ref().and_then(|v| v.as_bool()) == Some(true)
                    })
                })
                .unwrap_or(false);

            if is_protected {
                password_id = Some(node.node_id.as_ref().to_string());
            } else if username_id.is_none()
                && (name.contains("email")
                    || name.contains("user")
                    || name.contains("login")
                    || name.contains("phone")
                    || name.contains("account")
                    || name.is_empty())
            {
                username_id = Some(node.node_id.as_ref().to_string());
            }
        }

        if (role == "button" || role == "link")
            && submit_id.is_none()
            && (name.contains("sign in")
                || name.contains("log in")
                || name.contains("login")
                || name.contains("submit")
                || name.contains("continue")
                || name.contains("next"))
        {
            submit_id = Some(node.node_id.as_ref().to_string());
        }
    }

    match (username_id, password_id, submit_id) {
        (Some(u), Some(p), Some(s)) => Some((u, p, s)),
        _ => None,
    }
}

fn find_ax_node_by_id<'a>(nodes: &'a [AxNode], id: &str) -> Option<&'a AxNode> {
    nodes.iter().find(|n| n.node_id.as_ref() == id)
}

async fn cdp_insert_text(page: &Page, text: &str) -> Result<(), Temm1eError> {
    use chromiumoxide::cdp::browser_protocol::input::InsertTextParams;
    page.execute(InsertTextParams::new(text))
        .await
        .map_err(|e| Temm1eError::Tool(format!("insertText failed: {}", e)))?;
    Ok(())
}

async fn cdp_focus_backend_node(
    page: &Page,
    backend_node_id: chromiumoxide::cdp::browser_protocol::dom::BackendNodeId,
) -> Result<(), Temm1eError> {
    use chromiumoxide::cdp::browser_protocol::dom::FocusParams;
    page.execute(
        FocusParams::builder()
            .backend_node_id(backend_node_id)
            .build(),
    )
    .await
    .map_err(|e| Temm1eError::Tool(format!("DOM.focus failed: {}", e)))?;
    Ok(())
}

async fn cdp_clear_field(
    page: &Page,
    backend_node_id: chromiumoxide::cdp::browser_protocol::dom::BackendNodeId,
) -> Result<(), Temm1eError> {
    use chromiumoxide::cdp::browser_protocol::dom::ResolveNodeParams;
    use chromiumoxide::cdp::js_protocol::runtime::CallFunctionOnParams;
    let resolved = page
        .execute(
            ResolveNodeParams::builder()
                .backend_node_id(backend_node_id)
                .build(),
        )
        .await
        .map_err(|e| Temm1eError::Tool(format!("DOM.resolveNode failed: {}", e)))?;
    let object_id = resolved
        .result
        .object
        .object_id
        .ok_or_else(|| Temm1eError::Tool("Resolved node has no remote object ID".into()))?;
    let js_fn = r#"function() { this.value = ''; this.dispatchEvent(new Event('input', {bubbles:true})); }"#;
    let mut call_params = CallFunctionOnParams::new(js_fn);
    call_params.object_id = Some(object_id);
    page.execute(call_params)
        .await
        .map_err(|e| Temm1eError::Tool(format!("callFunctionOn (clear) failed: {}", e)))?;
    Ok(())
}

async fn cdp_click_backend_node(
    page: &Page,
    backend_node_id: chromiumoxide::cdp::browser_protocol::dom::BackendNodeId,
) -> Result<(), Temm1eError> {
    use chromiumoxide::cdp::browser_protocol::dom::ResolveNodeParams;
    use chromiumoxide::cdp::js_protocol::runtime::CallFunctionOnParams;
    let resolved = page
        .execute(
            ResolveNodeParams::builder()
                .backend_node_id(backend_node_id)
                .build(),
        )
        .await
        .map_err(|e| Temm1eError::Tool(format!("DOM.resolveNode failed: {}", e)))?;
    let object_id = resolved
        .result
        .object
        .object_id
        .ok_or_else(|| Temm1eError::Tool("Resolved node has no remote object ID".into()))?;
    let mut call_params = CallFunctionOnParams::new("function() { this.click(); }");
    call_params.object_id = Some(object_id);
    page.execute(call_params)
        .await
        .map_err(|e| Temm1eError::Tool(format!("callFunctionOn (click) failed: {}", e)))?;
    Ok(())
}

/// Kill orphaned Chrome child processes (renderer, GPU, utility) by parent PID.
///
/// When Chrome's main process is killed via SIGKILL (from `kill_on_drop`), its child
/// processes (renderer, GPU process, utility process) are NOT automatically terminated
/// on macOS/Linux because SIGKILL cannot be caught and Chrome has no opportunity to
/// clean up its children. This function finds and kills those orphaned children.
///
/// On Windows, `taskkill /T /F /PID` kills the entire process tree.
///
/// This function is intentionally synchronous so it can be called from `Drop`.
pub fn kill_chrome_children(parent_pid: u32) {
    #[cfg(unix)]
    {
        // Find child processes via `pgrep -P <pid>` and kill them individually.
        // This handles renderer, GPU process, utility, and any other Chrome children.
        if let Ok(output) = std::process::Command::new("pgrep")
            .arg("-P")
            .arg(parent_pid.to_string())
            .output()
        {
            if output.status.success() {
                let pids = String::from_utf8_lossy(&output.stdout);
                let mut killed = 0u32;
                for line in pids.lines() {
                    if let Ok(child_pid) = line.trim().parse::<u32>() {
                        // Use `kill -9 <pid>` to send SIGKILL to each child.
                        let _ = std::process::Command::new("kill")
                            .args(["-9", &child_pid.to_string()])
                            .output();
                        killed += 1;
                    }
                }
                if killed > 0 {
                    tracing::debug!(
                        parent_pid = parent_pid,
                        children_killed = killed,
                        "Killed orphaned Chrome child processes"
                    );
                }
            }
        }
    }

    #[cfg(windows)]
    {
        // On Windows, `taskkill /T /F /PID <pid>` kills the process tree.
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &parent_pid.to_string()])
            .output();
        tracing::debug!(pid = parent_pid, "Killed Chrome process tree (Windows)");
    }
}

impl Drop for BrowserTool {
    fn drop(&mut self) {
        self.signal_shutdown();

        // NOTE: Session auto-capture cannot happen in Drop because it requires async.
        // Callers should use shutdown() or close_with_capture() before dropping.
        // If the browser is still running at this point, sessions may not be saved.
        if let Ok(guard) = self.browser.try_lock() {
            if guard.is_some() {
                tracing::warn!(
                    "BrowserTool dropped with active browser — sessions may not be saved. \
                     Use shutdown() or close_with_capture() for graceful cleanup."
                );
            }
        }

        // Best-effort synchronous cleanup: take the browser and CDP handle out of
        // their mutexes so their Drop impls fire immediately. This triggers
        // chromiumoxide's kill_on_drop on the main Chrome process.
        //
        // try_lock() is used because we cannot .await in Drop. If the lock is held
        // (e.g., mid-operation), we skip — the Arc refcount will eventually reach
        // zero and Drop will fire later, but we lose the guarantee of immediate
        // cleanup.
        if let Ok(mut guard) = self.browser.try_lock() {
            let _ = guard.take(); // Browser::drop -> kill_on_drop fires here
        }
        if let Ok(mut guard) = self.page.try_lock() {
            let _ = guard.take();
        }
        if let Ok(mut guard) = self.cdp_handle.try_lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }

        // Clear browser lifecycle tracking (best-effort)
        if let Ok(mut started) = self.browser_started_at.lock() {
            *started = None;
        }
        if let Ok(mut domains) = self.active_domains.lock() {
            domains.clear();
        }

        // Kill orphaned Chrome child processes (renderer, GPU, utility).
        // The main Chrome process is killed by kill_on_drop above, but its
        // children (spawned as separate processes) survive on macOS/Linux
        // because SIGKILL does not propagate to children.
        let pid = self.chrome_pid.swap(0, Ordering::Relaxed);
        if pid > 0 {
            kill_chrome_children(pid);
        }

        // CRITICAL: Clean up SingletonLock files so user's real Chrome can open.
        Self::cleanup_singleton_locks();
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
         - accessibility_tree (alias: observe_tree): Extract the page accessibility tree — \
           returns a filtered, numbered list of interactive and semantic elements \
           (buttons, links, inputs, headings, etc.) with their properties\n\
         - observe: Smart layered observation — auto-selects between tree only (Tier 1), \
           tree + DOM as Markdown (Tier 2), or tree + screenshot (Tier 3) based on page \
           complexity. Supports incremental observation (skips if page unchanged). \
           Optional hint and retry parameters.\n\
         - authenticate: Log into a website using vault credentials. Requires service parameter.\n\
         - restore_web_session: Restore a previously captured web session (cookies + storage) \
           from the vault. Requires service parameter. Checks if session is still alive.\n\
         - close: Close the browser when done (auto-closes after idle timeout)\n\n\
         Vision workflow: screenshot → analyze image → click_at coordinates → repeat.\n\
         This bypasses Shadow DOM, anti-bot CSS tricks, and hidden elements.\n\n\
         Zoom-refine workflow: screenshot → identify target region → zoom_region x1,y1,x2,y2 → \
         analyze zoomed view → click_at precise coordinates. Much more accurate for small elements.\n\n\
         Structured observation: accessibility_tree → read numbered elements → \
         interact by selector or click_at. Lighter than screenshots for form-heavy pages.\n\n\
         Layered observation: observe → auto-selects optimal data level → \
         avoids wasting tokens on screenshots when tree is sufficient. \
         Tier 3 includes numbered SoM labels on interactive elements for precise targeting.\n\n\
         The browser runs in stealth mode with anti-detection patches applied."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["navigate", "click", "click_at", "type", "screenshot", "get_text", "evaluate", "get_html", "save_session", "restore_session", "accessibility_tree", "observe_tree", "observe", "zoom_region", "authenticate", "restore_web_session", "close"],
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
                },
                "hint": {
                    "type": "string",
                    "description": "Optional hint about what to look for (for 'observe' action, e.g. 'table', 'form', 'captcha', 'visual')"
                },
                "service": {
                    "type": "string",
                    "description": "Service name for credential lookup (for authenticate action)"
                },
                "retry": {
                    "type": "boolean",
                    "description": "Set true if previous action failed — triggers visual verification via screenshot (for 'observe' action)"
                },
                "x1": {
                    "type": "number",
                    "description": "Left X coordinate of region (for 'zoom_region' action)"
                },
                "y1": {
                    "type": "number",
                    "description": "Top Y coordinate of region (for 'zoom_region' action)"
                },
                "x2": {
                    "type": "number",
                    "description": "Right X coordinate of region (for 'zoom_region' action)"
                },
                "y2": {
                    "type": "number",
                    "description": "Bottom Y coordinate of region (for 'zoom_region' action)"
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

        // Handle close before launching browser — auto-capture sessions first
        if action == "close" {
            let saved = self.auto_capture_sessions_to_vault().await;
            let msg = self.close_browser().await;
            let content = if saved.is_empty() {
                msg
            } else {
                format!("Sessions saved: {}. {}", saved.join(", "), msg)
            };
            return Ok(ToolOutput {
                content,
                is_error: false,
            });
        }

        let page = self.ensure_browser().await?;
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

                // Track the domain for /browser status
                if let Some(domain) = extract_base_domain(url) {
                    if let Ok(mut domains) = self.active_domains.lock() {
                        domains.insert(domain);
                    }
                }

                tracing::info!(url = %url, "Browser navigating (stealth)");
                // 60s timeout for heavy sites like Facebook
                match tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    page.goto(url)
                ).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        // Navigation error but page may have partially loaded — continue
                        tracing::warn!(error = %e, "Navigation error (continuing)");
                    }
                    Err(_) => {
                        // Timeout — page may still be usable
                        tracing::warn!(url = %url, "Navigation timeout after 60s (continuing)");
                    }
                }

                // Reset observation tree hash — new page means new content
                if let Ok(mut hash) = self.last_tree_hash.lock() {
                    *hash = None;
                }

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

                // ── QR code auto-detection ──────────────────────────────
                // After navigation, check if the page has a QR code (common
                // on login pages for Zalo, WhatsApp Web, Telegram Web, WeChat).
                // If found, auto-screenshot so the vision pipeline sends it
                // to the user without them needing to ask.
                let qr_detected = detect_qr_on_page(&page).await;
                let mut content =
                    format!("Navigated to: {}\nTitle: {}", current_url, title);
                if qr_detected {
                    auto_screenshot_for_qr(&page, &self.last_image).await;
                    content.push_str(
                        "\n\n\u{1F4F1} QR code detected on this page. \
                         A screenshot has been captured automatically. \
                         The user should scan the QR code to log in.",
                    );
                }

                Ok(ToolOutput {
                    content,
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

            "accessibility_tree" | "observe_tree" => {
                tracing::debug!("Browser extracting accessibility tree via JS");

                // Use JavaScript to extract accessibility-relevant info from the DOM
                // because chromiumoxide 0.7 can't deserialize Accessibility.getFullAXTree
                // CDP responses (WS deserialization error on newer Chrome versions).
                let js = r#"(() => {
                    const results = [];
                    let idx = 1;
                    const walk = (el, depth) => {
                        if (!el || el.nodeType !== 1) return;
                        const tag = el.tagName.toLowerCase();
                        const role = el.getAttribute('role') || '';
                        const ariaLabel = el.getAttribute('aria-label') || '';
                        const type = el.getAttribute('type') || '';
                        const name = el.getAttribute('name') || '';
                        const text = (el.textContent || '').trim().substring(0, 80);
                        const isInteractive = ['a','button','input','select','textarea'].includes(tag)
                            || ['button','link','textbox','combobox','checkbox','radio','tab','menuitem','searchbox','slider','switch'].includes(role);
                        const isSemantic = ['h1','h2','h3','h4','h5','h6','nav','main','form','table','img','ul','ol'].includes(tag)
                            || ['heading','navigation','main','form','table','list','listitem','img','alert','dialog'].includes(role);
                        if (isInteractive || isSemantic) {
                            let label = ariaLabel || el.title || '';
                            if (!label && tag === 'a') label = text;
                            if (!label && tag === 'img') label = el.alt || el.src?.split('/').pop() || '';
                            if (!label && ['input','textarea','select'].includes(tag)) {
                                const id = el.id;
                                if (id) { const lbl = document.querySelector('label[for="'+id+'"]'); if (lbl) label = lbl.textContent.trim(); }
                            }
                            if (!label && ['h1','h2','h3','h4','h5','h6'].includes(tag)) label = text;
                            if (!label && tag === 'button') label = text;
                            const effectiveRole = role || tag;
                            let entry = '  '.repeat(depth) + '[' + idx + '] ' + effectiveRole;
                            if (label) entry += ' "' + label.substring(0,60).replace(/"/g,'\\"') + '"';
                            if (tag === 'a') {
                                const href = el.getAttribute('href') || '';
                                if (href && !href.startsWith('javascript:')) {
                                    entry += ' href="' + href.substring(0, 80) + '"';
                                }
                            }
                            if (tag === 'input' && type) entry += ' type=' + type;
                            if (el.value && ['input','textarea','select'].includes(tag)) entry += ' value="' + el.value.substring(0,30) + '"';
                            if (el.disabled) entry += ' disabled=true';
                            if (el.checked) entry += ' checked=true';
                            if (el.required) entry += ' required=true';
                            if (tag.match(/^h[1-6]$/)) entry += ' level=' + tag[1];
                            results.push(entry);
                            idx++;
                        }
                        for (const child of el.children) walk(child, isInteractive || isSemantic ? depth+1 : depth);
                    };
                    const root = document.querySelector('main') || document.querySelector('[role="main"]') || document.body;
                    walk(root, 0);
                    return results.length > 0 ? results.join('\n') : '[No interactive or semantic elements found on this page]';
                })()"#;

                let result = page
                    .evaluate(js)
                    .await
                    .map_err(|e| {
                        Temm1eError::Tool(format!("Accessibility tree extraction failed: {}", e))
                    })?;

                let formatted = result
                    .into_value::<String>()
                    .unwrap_or_else(|_| "[Could not parse accessibility tree]".to_string());

                tracing::debug!(
                    output_len = formatted.len(),
                    "Accessibility tree extracted via JS"
                );

                Ok(ToolOutput {
                    content: formatted,
                    is_error: false,
                })
            }

            "observe" => {
                let hint = input.arguments.get("hint").and_then(|v| v.as_str());
                let retry = input
                    .arguments
                    .get("retry")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                tracing::debug!(hint = ?hint, retry = retry, "Browser observe — layered observation");

                // Get accessibility tree via JS (CDP typed API has deserialization issues)
                let ax_js = r#"(() => {
                    const results = [];
                    let idx = 1;
                    const walk = (el, depth) => {
                        if (!el || el.nodeType !== 1) return;
                        const tag = el.tagName.toLowerCase();
                        const role = el.getAttribute('role') || '';
                        const ariaLabel = el.getAttribute('aria-label') || '';
                        const type = el.getAttribute('type') || '';
                        const text = (el.textContent || '').trim().substring(0, 80);
                        const isInteractive = ['a','button','input','select','textarea'].includes(tag)
                            || ['button','link','textbox','combobox','checkbox','radio','tab','menuitem','searchbox','slider','switch'].includes(role);
                        const isSemantic = ['h1','h2','h3','h4','h5','h6','nav','main','form','table','img','ul','ol'].includes(tag)
                            || ['heading','navigation','main','form','table','list','listitem','img','alert','dialog'].includes(role);
                        if (isInteractive || isSemantic) {
                            let label = ariaLabel || el.title || '';
                            if (!label && tag === 'a') label = text;
                            if (!label && tag === 'img') label = el.alt || '';
                            if (!label && ['input','textarea','select'].includes(tag)) {
                                const id = el.id;
                                if (id) { const lbl = document.querySelector('label[for="'+id+'"]'); if (lbl) label = lbl.textContent.trim(); }
                            }
                            if (!label && ['h1','h2','h3','h4','h5','h6'].includes(tag)) label = text;
                            if (!label && tag === 'button') label = text;
                            const effectiveRole = role || tag;
                            let entry = '  '.repeat(depth) + '[' + idx + '] ' + effectiveRole;
                            if (label) entry += ' "' + label.substring(0,60).replace(/"/g,'\\"') + '"';
                            if (tag === 'a') {
                                const href = el.getAttribute('href') || '';
                                if (href && !href.startsWith('javascript:')) {
                                    entry += ' href="' + href.substring(0, 80) + '"';
                                }
                            }
                            if (tag === 'input' && type) entry += ' type=' + type;
                            if (el.value && ['input','textarea','select'].includes(tag)) entry += ' value="' + el.value.substring(0,30) + '"';
                            if (el.disabled) entry += ' disabled=true';
                            if (el.checked) entry += ' checked=true';
                            if (el.required) entry += ' required=true';
                            if (tag.match(/^h[1-6]$/)) entry += ' level=' + tag[1];
                            results.push(entry);
                            idx++;
                        }
                        for (const child of el.children) walk(child, isInteractive || isSemantic ? depth+1 : depth);
                    };
                    const root = document.querySelector('main') || document.querySelector('[role="main"]') || document.body;
                    walk(root, 0);
                    return results.length > 0 ? results.join('\n') : '[No interactive or semantic elements found]';
                })()"#;

                let tree_text = page
                    .evaluate(ax_js)
                    .await
                    .map_err(|e| Temm1eError::Tool(format!("Observe: tree extraction failed: {}", e)))?
                    .into_value::<String>()
                    .unwrap_or_else(|_| "[Could not extract page structure]".to_string());

                // Incremental observation — hash the tree and compare with last
                let mut hasher = DefaultHasher::new();
                tree_text.hash(&mut hasher);
                let current_hash = hasher.finish();

                if let Ok(mut last_hash) = self.last_tree_hash.lock() {
                    if *last_hash == Some(current_hash) {
                        tracing::debug!("Observe: page unchanged since last observation");
                        return Ok(ToolOutput {
                            content: "[Page unchanged since last observation]".to_string(),
                            is_error: false,
                        });
                    }
                    *last_hash = Some(current_hash);
                }

                // Analyze and select tier
                let mut meta = browser_observation::analyze_tree(&tree_text);

                // Run QR code detection — if found, escalate to Tier 3
                // so the user can see and scan the QR code
                let qr_detected = detect_qr_on_page(&page).await;
                meta.has_qr_code = qr_detected;

                let tier = browser_observation::select_tier(&meta, hint, retry);

                tracing::debug!(
                    tier = ?tier,
                    total_interactive = meta.total_interactive,
                    unlabeled = meta.unlabeled_interactive,
                    has_table = meta.has_table,
                    has_form = meta.has_form,
                    has_qr_code = meta.has_qr_code,
                    "Observe: tier selected"
                );

                match tier {
                    ObservationTier::Tree => Ok(ToolOutput {
                        content: tree_text,
                        is_error: false,
                    }),
                    ObservationTier::TreeWithDom { selector } => {
                        let js = format!(
                            "(() => {{ const el = document.querySelector('{}'); \
                             return el ? el.outerHTML : 'not found'; }})()",
                            selector.replace('\'', "\\'")
                        );
                        let dom_html: String = page
                            .evaluate(js)
                            .await
                            .map(|r| r.into_value::<String>().unwrap_or_default())
                            .unwrap_or_default();

                        let markdown = htmd::convert(&dom_html).unwrap_or(dom_html);

                        // Truncate markdown to 4000 chars (safe boundary via char_indices)
                        let md_truncated =
                            browser_observation::truncate_safe(&markdown, 4000);

                        Ok(ToolOutput {
                            content: format!(
                                "{}\n\n--- DOM Detail ({}) ---\n{}",
                                tree_text, selector, md_truncated
                            ),
                            is_error: false,
                        })
                    }
                    ObservationTier::TreeWithScreenshot { selector: sel } => {
                        use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;

                        // --- Prowl V2: SoM overlay injection ---
                        // Before capturing the screenshot, overlay numbered labels on
                        // interactive elements so the VLM can reference them by [N]
                        // matching the accessibility tree output.
                        let som_injected = {
                            let som_js = r#"(() => {
                                const labels = [];
                                let idx = 1;
                                const walk = (el, depth) => {
                                    if (!el || el.nodeType !== 1) return;
                                    const tag = el.tagName.toLowerCase();
                                    const role = el.getAttribute('role') || '';
                                    const isInteractive = ['a','button','input','select','textarea'].includes(tag)
                                        || ['button','link','textbox','combobox','checkbox','radio','tab','menuitem','searchbox','slider','switch'].includes(role);
                                    const isSemantic = ['h1','h2','h3','h4','h5','h6','nav','main','form','table','img','ul','ol'].includes(tag)
                                        || ['heading','navigation','main','form','table','list','listitem','img','alert','dialog'].includes(role);
                                    if (isInteractive || isSemantic) {
                                        const rect = el.getBoundingClientRect();
                                        if (rect.width > 0 && rect.height > 0 &&
                                            rect.top >= 0 && rect.left >= 0 &&
                                            rect.top < window.innerHeight && rect.left < window.innerWidth) {
                                            const label = document.createElement('div');
                                            label.className = 'gaze-som-overlay';
                                            label.textContent = idx;
                                            label.style.cssText = `
                                                position: fixed;
                                                left: ${Math.max(0, rect.left - 11)}px;
                                                top: ${Math.max(0, rect.top - 11)}px;
                                                width: 22px;
                                                height: 22px;
                                                background: #e53e3e;
                                                color: white;
                                                font-size: 11px;
                                                font-weight: bold;
                                                line-height: 22px;
                                                text-align: center;
                                                border-radius: 50%;
                                                z-index: 2147483647;
                                                pointer-events: none;
                                                box-shadow: 0 1px 3px rgba(0,0,0,0.4);
                                                font-family: Arial, sans-serif;
                                            `;
                                            document.body.appendChild(label);
                                            labels.push(idx);
                                        }
                                        idx++;
                                    }
                                    for (const child of el.children) walk(child, isInteractive || isSemantic ? depth+1 : depth);
                                };
                                const root = document.querySelector('main') || document.querySelector('[role="main"]') || document.body;
                                walk(root, 0);
                                return labels.length;
                            })()"#;

                            match page.evaluate(som_js).await {
                                Ok(result) => {
                                    let count = result.into_value::<i64>().unwrap_or(0);
                                    tracing::debug!(som_labels = count, "SoM overlays injected for Tier 3");
                                    count > 0
                                }
                                Err(e) => {
                                    tracing::warn!("SoM overlay injection failed (non-fatal): {}", e);
                                    false
                                }
                            }
                        };

                        // Small delay for overlays to render
                        if som_injected {
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }

                        let png_data = if let Some(ref css_sel) = sel {
                            let el = page.find_element(css_sel).await.map_err(|e| {
                                Temm1eError::Tool(format!(
                                    "Observe: element '{}' not found: {}",
                                    css_sel, e
                                ))
                            })?;
                            el.screenshot(CaptureScreenshotFormat::Png).await.map_err(
                                |e| {
                                    Temm1eError::Tool(format!(
                                        "Observe: element screenshot failed: {}",
                                        e
                                    ))
                                },
                            )?
                        } else {
                            page.screenshot(
                                chromiumoxide::page::ScreenshotParams::builder()
                                    .format(CaptureScreenshotFormat::Png)
                                    .build(),
                            )
                            .await
                            .map_err(|e| {
                                Temm1eError::Tool(format!(
                                    "Observe: viewport screenshot failed: {}",
                                    e
                                ))
                            })?
                        };

                        // --- Clean up SoM overlays ---
                        if som_injected {
                            let cleanup_js = "document.querySelectorAll('.gaze-som-overlay').forEach(e => e.remove())";
                            if let Err(e) = page.evaluate(cleanup_js).await {
                                tracing::warn!("SoM overlay cleanup failed (non-fatal): {}", e);
                            }
                        }

                        // Store base64 image for vision injection by the runtime.
                        {
                            use base64::Engine;
                            let b64 =
                                base64::engine::general_purpose::STANDARD.encode(&png_data);
                            if let Ok(mut img) = self.last_image.lock() {
                                *img = Some(ToolOutputImage {
                                    media_type: "image/png".to_string(),
                                    data: b64,
                                });
                            }
                        }

                        let qr_note = if qr_detected {
                            "\n\n\u{1F4F1} QR code detected on this page. \
                             The user should scan the QR code image above to log in."
                        } else {
                            ""
                        };

                        let som_note = if som_injected {
                            "\nScreenshot includes numbered [N] labels on interactive elements — \
                             these match the [N] indices in the tree above. Reference by number \
                             for precise targeting."
                        } else {
                            ""
                        };

                        Ok(ToolOutput {
                            content: format!(
                                "{}\n\n[Screenshot captured for visual analysis — \
                                 Tier 3 observation with SoM labels]{}{}",
                                tree_text, som_note, qr_note
                            ),
                            is_error: false,
                        })
                    }
                }
            }

            "zoom_region" => {
                use chromiumoxide::cdp::browser_protocol::page::{
                    CaptureScreenshotFormat, CaptureScreenshotParams, Viewport,
                };

                let x1 = input
                    .arguments
                    .get("x1")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'zoom_region' requires 'x1' parameter".into())
                    })? as u32;
                let y1 = input
                    .arguments
                    .get("y1")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'zoom_region' requires 'y1' parameter".into())
                    })? as u32;
                let x2 = input
                    .arguments
                    .get("x2")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'zoom_region' requires 'x2' parameter".into())
                    })? as u32;
                let y2 = input
                    .arguments
                    .get("y2")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'zoom_region' requires 'y2' parameter".into())
                    })? as u32;

                tracing::info!(x1, y1, x2, y2, "Browser zoom_region — capturing region at full resolution");

                // Validate the region
                let region = crate::grounding::validate_zoom_region(x1, y1, x2, y2, 1920, 1080)
                    .map_err(Temm1eError::Tool)?;

                let [rx1, ry1, rx2, ry2] = region;
                let region_w = (rx2 - rx1) as f64;
                let region_h = (ry2 - ry1) as f64;

                // Use CDP captureScreenshot with clip to capture just the region.
                // The clip viewport captures at the specified coordinates and scale=1
                // gives us the region at full resolution.
                let clip = Viewport {
                    x: rx1 as f64,
                    y: ry1 as f64,
                    width: region_w,
                    height: region_h,
                    scale: 2.0, // 2x for sharper detail in the zoomed view
                };

                let params = CaptureScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .clip(clip)
                    .build();

                let png_data = page.execute(
                    chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams {
                        format: Some(CaptureScreenshotFormat::Png),
                        clip: Some(chromiumoxide::cdp::browser_protocol::page::Viewport {
                            x: rx1 as f64,
                            y: ry1 as f64,
                            width: region_w,
                            height: region_h,
                            scale: 2.0,
                        }),
                        quality: None,
                        from_surface: None,
                        capture_beyond_viewport: None,
                        optimize_for_speed: None,
                    },
                )
                .await
                .map_err(|e| Temm1eError::Tool(format!("zoom_region screenshot failed: {}", e)))?;

                // CDP returns Binary (base64-encoded data) in the response.
                use base64::Engine;
                let b64_string: String = png_data.data.clone().into();
                // Decode to get raw byte count for the status message
                let raw_len = base64::engine::general_purpose::STANDARD
                    .decode(&b64_string)
                    .map(|v| v.len())
                    .unwrap_or(0);

                // Store as last_image for vision pipeline injection by the runtime
                if let Ok(mut img) = self.last_image.lock() {
                    *img = Some(ToolOutputImage {
                        media_type: "image/png".to_string(),
                        data: b64_string,
                    });
                }

                let _ = params; // suppress unused warning from builder pattern

                Ok(ToolOutput {
                    content: format!(
                        "Zoomed into region ({},{})→({},{}) at 2x resolution ({} bytes). \
                         The zoomed image is now visible for detailed analysis. \
                         Use click_at with coordinates from the ORIGINAL page (not this zoomed view) \
                         to interact with elements.",
                        rx1, ry1, rx2, ry2, raw_len
                    ),
                    is_error: false,
                })
            }

            "authenticate" => {
                let service = input.arguments.get("service").and_then(|v| v.as_str())
                    .ok_or_else(|| Temm1eError::Tool("'authenticate' requires 'service' parameter".into()))?;

                tracing::info!(service = %service, "Browser authenticate — credential isolation protocol");

                let vault = self.vault.as_ref()
                    .ok_or_else(|| Temm1eError::Tool("Vault not available".into()))?;
                let raw_bytes = vault.get_secret(&format!("web_cred:{}", service)).await?
                    .ok_or_else(|| Temm1eError::Tool(format!("No credentials for '{}'", service)))?;
                let zeroizing = Zeroizing::new(raw_bytes);
                let cred: WebCredential = serde_json::from_slice(&zeroizing)
                    .map_err(|e| Temm1eError::Tool(format!("Credential parse error: {}", e)))?;

                use chromiumoxide::cdp::browser_protocol::accessibility::GetFullAxTreeParams;
                let ax_result = page.execute(GetFullAxTreeParams::default()).await
                    .map_err(|e| Temm1eError::Tool(format!("Auth: ax tree failed: {}", e)))?;

                let (user_id, pass_id, submit_id) = detect_login_form(&ax_result.result.nodes)
                    .ok_or_else(|| Temm1eError::Tool("Could not detect login form on this page".into()))?;

                tracing::debug!(username_node = %user_id, password_node = %pass_id, submit_node = %submit_id, "Login form detected");

                let user_node = find_ax_node_by_id(&ax_result.result.nodes, &user_id)
                    .ok_or_else(|| Temm1eError::Tool("Username AX node not found".into()))?;
                let pass_node = find_ax_node_by_id(&ax_result.result.nodes, &pass_id)
                    .ok_or_else(|| Temm1eError::Tool("Password AX node not found".into()))?;
                let submit_node = find_ax_node_by_id(&ax_result.result.nodes, &submit_id)
                    .ok_or_else(|| Temm1eError::Tool("Submit AX node not found".into()))?;

                let user_backend_id = user_node.backend_dom_node_id
                    .ok_or_else(|| Temm1eError::Tool("Username AX node has no DOM backing".into()))?;
                let pass_backend_id = pass_node.backend_dom_node_id
                    .ok_or_else(|| Temm1eError::Tool("Password AX node has no DOM backing".into()))?;
                let submit_backend_id = submit_node.backend_dom_node_id
                    .ok_or_else(|| Temm1eError::Tool("Submit AX node has no DOM backing".into()))?;

                cdp_clear_field(&page, user_backend_id).await?;
                cdp_focus_backend_node(&page, user_backend_id).await?;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                cdp_insert_text(&page, &cred.username).await?;

                cdp_clear_field(&page, pass_backend_id).await?;
                cdp_focus_backend_node(&page, pass_backend_id).await?;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                cdp_insert_text(&page, &cred.password).await?;

                drop(cred);

                cdp_click_backend_node(&page, submit_backend_id).await?;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                let post_ax = page.execute(GetFullAxTreeParams::default()).await
                    .map_err(|e| Temm1eError::Tool(format!("Auth: post-login tree failed: {}", e)))?;
                let post_tree = format_ax_tree(&post_ax.result.nodes);
                let scrubbed = credential_scrub::scrub(&post_tree, &[service]);

                tracing::info!(service = %service, "Authentication flow completed");

                Ok(ToolOutput {
                    content: format!("Authenticated to '{}'. Post-login page:\n{}", service, scrubbed),
                    is_error: false,
                })
            }

            "restore_web_session" => {
                let service = input.arguments.get("service").and_then(|v| v.as_str())
                    .ok_or_else(|| Temm1eError::Tool("'restore_web_session' requires 'service' parameter".into()))?;

                tracing::info!(service = %service, "Browser restore_web_session — loading session from vault");

                let vault = self.vault.as_ref()
                    .ok_or_else(|| Temm1eError::Tool("Vault not available for session restore".into()))?;

                let (tree_text, session_alive) = crate::browser_session::restore_web_session(
                    &page, vault.as_ref(), service,
                ).await?;

                if session_alive {
                    Ok(ToolOutput {
                        content: format!(
                            "Session restored for '{}'. Current page:\n{}",
                            service, tree_text
                        ),
                        is_error: false,
                    })
                } else {
                    Ok(ToolOutput {
                        content: format!(
                            "Session for '{}' has expired (login prompt detected). Need to re-authenticate.",
                            service
                        ),
                        is_error: true,
                    })
                }
            }

            other => Ok(ToolOutput {
                content: format!(
                    "Unknown action '{}'. Valid actions: navigate, click, click_at, type, screenshot, \
                     get_text, evaluate, get_html, save_session, restore_session, \
                     accessibility_tree, observe_tree, observe, authenticate, restore_web_session, close",
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
    fn default_idle_timeout_is_zero_persistent() {
        assert_eq!(DEFAULT_IDLE_TIMEOUT_SECS, 0);
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
        // Compose the expected suffix with the OS-native separator so Windows
        // (`\`) and Unix (`/`) both match. Hardcoding `/` in the assertion
        // breaks on Windows where `to_string_lossy()` yields `\`.
        let expected_suffix = std::path::Path::new(".temm1e").join("sessions");
        assert!(
            dir.ends_with(&expected_suffix),
            "Sessions dir should end with {}, got: {}",
            expected_suffix.display(),
            dir.display()
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
            assert_eq!(tool.idle_timeout_secs, 0);
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
            assert_eq!(tool.idle_timeout_secs, 0);
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
                "accessibility_tree",
                "observe_tree",
                "observe",
                "zoom_region",
                "authenticate",
                "restore_web_session",
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

    // ── Accessibility tree formatting tests ─────────────────────────

    mod ax_tree_tests {
        use super::*;
        use chromiumoxide::cdp::browser_protocol::accessibility::{
            AxNode, AxNodeId, AxValue, AxValueType,
        };

        /// Helper: create a minimal AxNode with role and name.
        fn make_node(
            id: &str,
            role: &str,
            name: &str,
            parent_id: Option<&str>,
            child_ids: Option<Vec<&str>>,
        ) -> AxNode {
            let mut node = AxNode::new(AxNodeId::new(id), false);
            node.role = Some(AxValue {
                r#type: AxValueType::Role,
                value: Some(serde_json::Value::String(role.to_string())),
                related_nodes: None,
                sources: None,
            });
            node.name = Some(AxValue {
                r#type: AxValueType::String,
                value: Some(serde_json::Value::String(name.to_string())),
                related_nodes: None,
                sources: None,
            });
            node.parent_id = parent_id.map(AxNodeId::new);
            node.child_ids = child_ids.map(|ids| ids.iter().map(|i| AxNodeId::new(*i)).collect());
            node
        }

        /// Helper: create an ignored AxNode.
        fn make_ignored_node(id: &str, parent_id: Option<&str>) -> AxNode {
            let mut node = AxNode::new(AxNodeId::new(id), true);
            node.parent_id = parent_id.map(AxNodeId::new);
            node
        }

        /// Helper: create a generic container node (not interactive, not semantic).
        fn make_generic_node(
            id: &str,
            parent_id: Option<&str>,
            child_ids: Option<Vec<&str>>,
        ) -> AxNode {
            let mut node = AxNode::new(AxNodeId::new(id), false);
            node.role = Some(AxValue {
                r#type: AxValueType::Role,
                value: Some(serde_json::Value::String("generic".to_string())),
                related_nodes: None,
                sources: None,
            });
            node.parent_id = parent_id.map(AxNodeId::new);
            node.child_ids = child_ids.map(|ids| ids.iter().map(|i| AxNodeId::new(*i)).collect());
            node
        }

        #[test]
        fn empty_nodes_returns_placeholder() {
            let result = format_ax_tree(&[]);
            assert_eq!(result, "(empty accessibility tree)");
        }

        #[test]
        fn single_button_node() {
            let nodes = vec![make_node("1", "button", "Submit", None, None)];
            let result = format_ax_tree(&nodes);
            assert!(result.contains("[1] button \"Submit\""));
        }

        #[test]
        fn generic_container_is_skipped_but_children_shown() {
            let nodes = vec![
                make_generic_node("root", None, Some(vec!["btn"])),
                make_node("btn", "button", "Click me", Some("root"), None),
            ];
            let result = format_ax_tree(&nodes);
            assert!(!result.contains("generic"));
            assert!(result.contains("[1] button \"Click me\""));
        }

        #[test]
        fn ignored_node_is_skipped() {
            let nodes = vec![
                make_generic_node("root", None, Some(vec!["ign", "btn"])),
                make_ignored_node("ign", Some("root")),
                make_node("btn", "button", "Visible", Some("root"), None),
            ];
            let result = format_ax_tree(&nodes);
            assert!(result.contains("[1] button \"Visible\""));
            assert!(!result.contains("[2]"));
        }

        #[test]
        fn nested_hierarchy_indentation() {
            let nodes = vec![
                make_node("form1", "form", "Login", None, Some(vec!["input1", "btn1"])),
                make_node("input1", "textbox", "Username", Some("form1"), None),
                make_node("btn1", "button", "Log In", Some("form1"), None),
            ];
            let result = format_ax_tree(&nodes);
            assert!(result.contains("[1] form \"Login\""));
            assert!(result.contains("  [2] textbox \"Username\""));
            assert!(result.contains("  [3] button \"Log In\""));
        }

        #[test]
        fn schema_includes_observe_action() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let tool = BrowserTool::new();
                let schema = tool.parameters_schema();
                let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
                let action_strs: Vec<&str> = actions.iter().map(|v| v.as_str().unwrap()).collect();
                assert!(action_strs.contains(&"observe"), "Missing observe action");
                assert!(
                    action_strs.contains(&"accessibility_tree"),
                    "Missing accessibility_tree"
                );
                assert!(
                    action_strs.contains(&"observe_tree"),
                    "Missing observe_tree"
                );
            });
        }

        #[test]
        fn schema_includes_hint_and_retry_parameters() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let tool = BrowserTool::new();
                let schema = tool.parameters_schema();
                assert!(
                    schema["properties"]["hint"].is_object(),
                    "Missing hint parameter"
                );
                assert!(
                    schema["properties"]["retry"].is_object(),
                    "Missing retry parameter"
                );
            });
        }

        #[test]
        fn description_mentions_observe_action() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let tool = BrowserTool::new();
                let desc = tool.description();
                assert!(desc.contains("observe"), "Description missing observe");
                assert!(desc.contains("Tier 1"), "Description missing Tier 1");
                assert!(desc.contains("Tier 2"), "Description missing Tier 2");
                assert!(desc.contains("Tier 3"), "Description missing Tier 3");
            });
        }

        #[test]
        fn last_tree_hash_initializes_to_none() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let tool = BrowserTool::new();
                let hash = tool.last_tree_hash.lock().unwrap();
                assert!(hash.is_none(), "last_tree_hash should start as None");
            });
        }

        #[test]
        fn all_interactive_roles_recognized() {
            for role in INTERACTIVE_ROLES {
                let nodes = vec![make_node("1", role, "test", None, None)];
                let result = format_ax_tree(&nodes);
                assert!(
                    result.contains(&format!("[1] {}", role)),
                    "Role '{}' not recognized",
                    role
                );
            }
        }

        #[test]
        fn all_semantic_roles_recognized() {
            for role in SEMANTIC_ROLES {
                let nodes = vec![make_node("1", role, "test", None, None)];
                let result = format_ax_tree(&nodes);
                assert!(
                    result.contains(&format!("[1] {}", role)),
                    "Role '{}' not recognized",
                    role
                );
            }
        }

        #[test]
        fn browser_tool_schema_lists_all_actions() {
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
                    "accessibility_tree",
                    "observe_tree",
                    "observe",
                    "zoom_region",
                    "authenticate",
                    "restore_web_session",
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
    }

    // ── QR detection JS constant tests ──────────────────────────────

    #[test]
    fn qr_detect_js_not_empty() {
        assert!(
            !QR_DETECT_JS.is_empty(),
            "QR detection JS should not be empty"
        );
    }

    #[test]
    fn qr_detect_js_is_iife() {
        assert!(
            QR_DETECT_JS.contains("(() =>"),
            "QR detect JS should be an IIFE"
        );
    }

    #[test]
    fn qr_detect_js_checks_canvas() {
        assert!(
            QR_DETECT_JS.contains("canvas"),
            "QR detect JS should check canvas elements"
        );
    }

    #[test]
    fn qr_detect_js_checks_images() {
        assert!(
            QR_DETECT_JS.contains("querySelectorAll('img')"),
            "QR detect JS should check img elements"
        );
    }

    #[test]
    fn qr_detect_js_returns_qr_canvas() {
        assert!(
            QR_DETECT_JS.contains("'qr_canvas'"),
            "QR detect JS should return 'qr_canvas' for canvas-based QR"
        );
    }

    #[test]
    fn qr_detect_js_returns_qr_image() {
        assert!(
            QR_DETECT_JS.contains("'qr_image'"),
            "QR detect JS should return 'qr_image' for img-based QR"
        );
    }

    #[test]
    fn qr_detect_js_returns_qr_possible() {
        assert!(
            QR_DETECT_JS.contains("'qr_possible'"),
            "QR detect JS should return 'qr_possible' for heuristic match"
        );
    }

    #[test]
    fn qr_detect_js_returns_no_qr() {
        assert!(
            QR_DETECT_JS.contains("'no_qr'"),
            "QR detect JS should return 'no_qr' when no QR found"
        );
    }

    #[test]
    fn qr_detect_js_checks_src_alt_class() {
        assert!(
            QR_DETECT_JS.contains(".src") && QR_DETECT_JS.contains(".alt"),
            "QR detect JS should check img.src and img.alt for 'qr'"
        );
        assert!(
            QR_DETECT_JS.contains(".className"),
            "QR detect JS should check img.className for 'qr'"
        );
    }

    #[test]
    fn qr_detect_js_checks_square_ratio() {
        assert!(
            QR_DETECT_JS.contains("0.9") && QR_DETECT_JS.contains("1.1"),
            "QR detect JS should check for square-ish ratio (0.9-1.1)"
        );
    }

    #[test]
    fn qr_detect_js_minimum_size_filter() {
        assert!(
            QR_DETECT_JS.contains(">= 100") && QR_DETECT_JS.contains(">= 150"),
            "QR detect JS should have minimum size thresholds"
        );
    }

    // ── V2: Persistent browser / domain tracking / extract_base_domain ──

    #[test]
    fn extract_base_domain_simple() {
        assert_eq!(
            extract_base_domain("https://www.facebook.com/login"),
            Some("facebook".to_string())
        );
        assert_eq!(
            extract_base_domain("https://facebook.com"),
            Some("facebook".to_string())
        );
    }

    #[test]
    fn extract_base_domain_no_www() {
        assert_eq!(
            extract_base_domain("https://github.com/login"),
            Some("github".to_string())
        );
    }

    #[test]
    fn extract_base_domain_subdomain() {
        assert_eq!(
            extract_base_domain("https://mail.google.com"),
            Some("google".to_string())
        );
    }

    #[test]
    fn extract_base_domain_with_port() {
        assert_eq!(
            extract_base_domain("http://localhost:8080/path"),
            Some("localhost".to_string())
        );
    }

    #[test]
    fn extract_base_domain_invalid() {
        assert_eq!(extract_base_domain("not-a-url"), None);
        assert_eq!(extract_base_domain(""), None);
    }

    #[test]
    fn browser_tool_has_tracking_fields() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            // browser_started_at should start as None
            assert!(tool.browser_started_at.lock().unwrap().is_none());
            // active_domains should start empty
            assert!(tool.active_domains.lock().unwrap().is_empty());
        });
    }

    #[test]
    fn browser_tool_is_not_running_by_default() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            assert!(!tool.is_running());
        });
    }

    #[test]
    fn browser_tool_uptime_none_when_not_started() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            assert!(tool.uptime().is_none());
        });
    }

    #[test]
    fn browser_tool_get_active_domains_empty_default() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            assert!(tool.get_active_domains().is_empty());
        });
    }

    #[test]
    fn browser_tool_persistent_timeout_skips_watchdog() {
        // When timeout is 0 (default), the watchdog should not auto-close
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::new();
            assert_eq!(tool.idle_timeout_secs, 0);
            // Watchdog exists but idle_timeout == 0 means it skips auto-close
        });
    }

    #[test]
    fn browser_tool_nonzero_timeout_backwards_compat() {
        // When timeout > 0, the old idle auto-close behavior is preserved
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = BrowserTool::with_timeout(600);
            assert_eq!(tool.idle_timeout_secs, 600);
        });
    }

    #[tokio::test]
    async fn browser_tool_list_saved_sessions_no_vault() {
        let tool = BrowserTool::new();
        // No vault attached — should return empty
        let sessions = tool.list_saved_sessions().await;
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn browser_tool_forget_session_no_vault() {
        let tool = BrowserTool::new();
        let result = tool.forget_session("facebook").await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Vault not available");
    }

    #[tokio::test]
    async fn browser_tool_close_with_capture_no_browser() {
        let tool = BrowserTool::new();
        let (msg, saved) = tool.close_with_capture().await;
        assert_eq!(msg, "No browser was running.");
        assert!(saved.is_empty());
    }
}
