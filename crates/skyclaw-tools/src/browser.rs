//! Browser tool — headless Chrome automation via Chrome DevTools Protocol.
//!
//! Provides the agent with browser actions: navigate, click, type, screenshot,
//! get page text, and evaluate JavaScript. Each tool call performs exactly one
//! action — the agent chains actions across rounds.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::Page;
use futures::StreamExt;
use skyclaw_core::types::error::SkyclawError;
use skyclaw_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};
use tokio::sync::Mutex;

/// Auto-close browser after this many seconds of inactivity.
const IDLE_TIMEOUT_SECS: i64 = 120;

/// Manages a shared browser instance with one active page.
/// Always runs headless — GUI mode deferred to a future patch.
pub struct BrowserTool {
    browser: Arc<Mutex<Option<Browser>>>,
    page: Arc<Mutex<Option<Page>>>,
    /// Unix timestamp of last browser action — used for idle auto-close.
    last_used: Arc<AtomicI64>,
}

impl BrowserTool {
    /// Create a new browser tool (always headless).
    pub fn new() -> Self {
        let browser = Arc::new(Mutex::new(None));
        let page = Arc::new(Mutex::new(None));
        let last_used = Arc::new(AtomicI64::new(0));

        // Spawn idle auto-close watchdog
        {
            let browser = browser.clone();
            let page = page.clone();
            let last_used = last_used.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    let lu = last_used.load(Ordering::Relaxed);
                    if lu == 0 {
                        continue; // never used yet
                    }
                    let now = chrono::Utc::now().timestamp();
                    if now - lu > IDLE_TIMEOUT_SECS {
                        let mut b = browser.lock().await;
                        let mut p = page.lock().await;
                        if b.is_some() {
                            tracing::info!("Browser idle for {}s — auto-closing", now - lu);
                            *p = None;
                            *b = None;
                            last_used.store(0, Ordering::Relaxed);
                        }
                    }
                }
            });
        }

        Self {
            browser,
            page,
            last_used,
        }
    }

    /// Close the browser and free resources.
    async fn close_browser(&self) -> String {
        let mut browser_guard = self.browser.lock().await;
        let mut page_guard = self.page.lock().await;
        if browser_guard.is_some() {
            *page_guard = None;
            *browser_guard = None;
            self.last_used.store(0, Ordering::Relaxed);
            tracing::info!("Browser closed by agent");
            "Browser closed.".to_string()
        } else {
            "No browser was running.".to_string()
        }
    }

    /// Lazily launch the browser on first use, or relaunch if dead.
    async fn ensure_browser(&self) -> Result<Page, SkyclawError> {
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
                }
            }
        }

        let mut config = BrowserConfig::builder();
        config = config
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--no-sandbox")
            .arg("--disable-dev-shm-usage")
            .window_size(1280, 900);

        let config = config.build().map_err(|e| {
            SkyclawError::Tool(format!("Failed to build browser config: {}", e))
        })?;

        let (browser, mut handler) = Browser::launch(config).await.map_err(|e| {
            SkyclawError::Tool(format!(
                "Failed to launch browser. Is Chrome/Chromium installed? Error: {}",
                e
            ))
        })?;

        // Spawn the CDP handler — this MUST keep running for the browser to work.
        // We hold the JoinHandle implicitly via the spawned task.
        tokio::spawn(async move {
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

        // Give the browser a moment to fully initialize
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let page = browser.new_page("about:blank").await.map_err(|e| {
            SkyclawError::Tool(format!("Failed to create page: {}", e))
        })?;

        // Wait for page to be ready
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        *browser_guard = Some(browser);
        *page_guard = Some(page.clone());
        self.last_used.store(chrono::Utc::now().timestamp(), Ordering::Relaxed);

        tracing::info!("Browser launched (headless)");
        Ok(page)
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "Control a Chrome browser to navigate websites, click elements, type text, \
         take screenshots, read page content, and run JavaScript. Each call performs \
         one action. Chain multiple calls across rounds for multi-step workflows.\n\n\
         Actions:\n\
         - navigate: Go to a URL\n\
         - click: Click an element by CSS selector\n\
         - type: Type text into an input field by CSS selector\n\
         - screenshot: Capture the page as a PNG (saved to workspace)\n\
         - get_text: Get the visible text content of the page\n\
         - evaluate: Execute JavaScript and return the result\n\
         - get_html: Get the raw HTML of the page or an element\n\
         - close: Close the browser when done (auto-closes after 2 min idle)"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["navigate", "click", "type", "screenshot", "get_text", "evaluate", "get_html", "close"],
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
                "text": {
                    "type": "string",
                    "description": "Text to type (for 'type' action)"
                },
                "script": {
                    "type": "string",
                    "description": "JavaScript code to execute (for 'evaluate' action)"
                },
                "filename": {
                    "type": "string",
                    "description": "Screenshot filename (for 'screenshot' action, defaults to 'screenshot.png')"
                }
            },
            "required": ["action"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: Vec::new(),
            network_access: vec!["*".to_string()],
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, SkyclawError> {
        let action = input
            .arguments
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SkyclawError::Tool("Missing required parameter: action".into()))?;

        // Handle close before launching browser
        if action == "close" {
            let msg = self.close_browser().await;
            return Ok(ToolOutput {
                content: msg,
                is_error: false,
            });
        }

        let page = self.ensure_browser().await?;
        self.last_used.store(chrono::Utc::now().timestamp(), Ordering::Relaxed);

        match action {
            "navigate" => {
                let url = input
                    .arguments
                    .get("url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        SkyclawError::Tool("'navigate' requires 'url' parameter".into())
                    })?;

                tracing::info!(url = %url, "Browser navigating");
                page.goto(url).await.map_err(|e| {
                    SkyclawError::Tool(format!("Navigation failed: {}", e))
                })?;

                // Wait for page to settle
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                let title = page
                    .get_title()
                    .await
                    .map_err(|e| SkyclawError::Tool(format!("Failed to get title: {}", e)))?
                    .unwrap_or_default();

                let current_url = page
                    .url()
                    .await
                    .map_err(|e| SkyclawError::Tool(format!("Failed to get URL: {}", e)))?
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
                        SkyclawError::Tool("'click' requires 'selector' parameter".into())
                    })?;

                tracing::info!(selector = %selector, "Browser clicking");
                let element = page.find_element(selector).await.map_err(|e| {
                    SkyclawError::Tool(format!(
                        "Element not found for selector '{}': {}",
                        selector, e
                    ))
                })?;

                element.click().await.map_err(|e| {
                    SkyclawError::Tool(format!("Click failed: {}", e))
                })?;

                // Wait for any navigation/updates
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                Ok(ToolOutput {
                    content: format!("Clicked element: {}", selector),
                    is_error: false,
                })
            }

            "type" => {
                let selector = input
                    .arguments
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        SkyclawError::Tool("'type' requires 'selector' parameter".into())
                    })?;
                let text = input
                    .arguments
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        SkyclawError::Tool("'type' requires 'text' parameter".into())
                    })?;

                tracing::info!(selector = %selector, "Browser typing");
                let element = page.find_element(selector).await.map_err(|e| {
                    SkyclawError::Tool(format!(
                        "Element not found for selector '{}': {}",
                        selector, e
                    ))
                })?;

                element.click().await.map_err(|e| {
                    SkyclawError::Tool(format!("Failed to focus element: {}", e))
                })?;

                element.type_str(text).await.map_err(|e| {
                    SkyclawError::Tool(format!("Type failed: {}", e))
                })?;

                Ok(ToolOutput {
                    content: format!(
                        "Typed {} chars into '{}'",
                        text.len(),
                        selector
                    ),
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
                let png_data = page.screenshot(
                    chromiumoxide::page::ScreenshotParams::builder()
                        .full_page(true)
                        .build(),
                ).await.map_err(|e| {
                    SkyclawError::Tool(format!("Screenshot failed: {}", e))
                })?;

                tokio::fs::write(&save_path, &png_data).await.map_err(|e| {
                    SkyclawError::Tool(format!("Failed to save screenshot: {}", e))
                })?;

                Ok(ToolOutput {
                    content: format!(
                        "Screenshot saved: {} ({} bytes)\nPath: {}",
                        safe_name,
                        png_data.len(),
                        save_path.display()
                    ),
                    is_error: false,
                })
            }

            "get_text" => {
                tracing::info!("Browser getting page text");

                let text: String = page
                    .evaluate("document.body.innerText")
                    .await
                    .map_err(|e| SkyclawError::Tool(format!("Failed to get text: {}", e)))?
                    .into_value()
                    .map_err(|e| SkyclawError::Tool(format!("Failed to parse text: {:?}", e)))?;

                // Truncate if too long
                let max_chars = 15_000;
                let truncated = if text.len() > max_chars {
                    format!(
                        "{}...\n\n[Truncated — {} total chars]",
                        &text[..max_chars],
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
                        SkyclawError::Tool("'evaluate' requires 'script' parameter".into())
                    })?;

                tracing::info!("Browser evaluating JavaScript");
                let result: serde_json::Value = page
                    .evaluate(script)
                    .await
                    .map_err(|e| SkyclawError::Tool(format!("JS evaluation failed: {}", e)))?
                    .into_value()
                    .map_err(|e| {
                        SkyclawError::Tool(format!("Failed to parse JS result: {:?}", e))
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
                        SkyclawError::Tool(format!(
                            "Element not found for selector '{}': {}",
                            sel, e
                        ))
                    })?;
                    let script = format!(
                        "document.querySelector('{}').outerHTML",
                        sel.replace('\'', "\\'")
                    );
                    page.evaluate(script)
                        .await
                        .map_err(|e| SkyclawError::Tool(format!("Failed to get HTML: {}", e)))?
                        .into_value()
                        .map_err(|e| {
                            SkyclawError::Tool(format!("Failed to parse HTML: {:?}", e))
                        })?
                } else {
                    page.evaluate("document.documentElement.outerHTML")
                        .await
                        .map_err(|e| SkyclawError::Tool(format!("Failed to get HTML: {}", e)))?
                        .into_value()
                        .map_err(|e| {
                            SkyclawError::Tool(format!("Failed to parse HTML: {:?}", e))
                        })?
                };

                // Truncate if too long
                let max_chars = 15_000;
                let truncated = if html.len() > max_chars {
                    format!(
                        "{}...\n\n[Truncated — {} total chars]",
                        &html[..max_chars],
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

            other => Ok(ToolOutput {
                content: format!(
                    "Unknown action '{}'. Valid actions: navigate, click, type, screenshot, get_text, evaluate, get_html, close",
                    other
                ),
                is_error: true,
            }),
        }
    }
}
