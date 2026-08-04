// tools/browser.rs — BrowserTool: Chromium 浏览器控制。
// Lazy init: 第一次调用时才启动浏览器，不静默。

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use base64::Engine;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams;
use chromiumoxide::cdp::browser_protocol::network::CookieParam;
use chromiumoxide::page::Page;
use futures::StreamExt;
use katherine_core::error::EngineError;
use katherine_core::tool::{PermissionLevel, Tool, ToolDefinition, ToolResult};

/// 找到 Chromium 路径。
/// 只查环境变量和本地预装路径——不自动下载。找不到就报错。
fn find_chrome() -> Result<PathBuf, EngineError> {
    if let Ok(p) = std::env::var("CHROME_PATH") {
        let path = PathBuf::from(&p);
        if path.exists() {
            return Ok(path);
        }
    }
    if let Ok(home) = std::env::var("KATHERINE_HOME") {
        let bundled = PathBuf::from(&home).join("bin").join("chrome-win").join("chrome.exe");
        if bundled.exists() {
            return Ok(bundled);
        }
    }
    Err(EngineError::Config(
        "Chromium not found. Set CHROME_PATH or place chrome.exe in KATHERINE_HOME/bin/chrome-win/".into()
    ))
}

/// 浏览器控制工具——Lazy init。
pub struct BrowserTool {
    chrome_path: PathBuf,
    browser: OnceLock<Arc<Browser>>,
}

impl BrowserTool {
    pub fn new() -> Result<Self, EngineError> {
        let chrome_path = find_chrome()?;
        Ok(BrowserTool {
            chrome_path,
            browser: OnceLock::new(),
        })
    }

    async fn get_browser(&self) -> Result<&Arc<Browser>, EngineError> {
        if let Some(b) = self.browser.get() {
            return Ok(b);
        }

        let user_data = std::env::var("KATHERINE_HOME")
            .map(|h| PathBuf::from(h).join("browser-profile"))
            .unwrap_or_else(|_| PathBuf::from("browser-profile"));

        eprintln!("Launching Chromium: {}", self.chrome_path.display());

        let config = BrowserConfig::builder()
            .chrome_executable(&self.chrome_path)
            .user_data_dir(user_data)
            .with_head()  // Selena 能看到浏览器窗口
            .args(vec![
                "--no-first-run".to_string(),
                "--disable-default-apps".to_string(),
                "--disable-popup-blocking".to_string(),
                "--disable-background-networking".to_string(),
                "--disable-sync".to_string(),
                "--no-default-browser-check".to_string(),
            ])
            .build()
            .map_err(|e| EngineError::Config(format!("BrowserConfig: {e}")))?;

        let (browser, mut handler) =
            Browser::launch(config).await.map_err(|e| {
                EngineError::Config(format!("Launch failed: {e}"))
            })?;

        tokio::spawn(async move {
            while let Some(_) = handler.next().await {}
        });

        let _ = self.browser.set(Arc::new(browser));
        Ok(self.browser.get().unwrap())
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "action")]
enum BrowserAction {
    #[serde(rename = "navigate")]
    Navigate { url: String },
    #[serde(rename = "screenshot")]
    Screenshot {
        #[serde(default)]
        selector: Option<String>,
    },
    #[serde(rename = "click")]
    Click { selector: String },
    #[serde(rename = "type_text")]
    TypeText { selector: String, text: String },
    #[serde(rename = "scroll")]
    Scroll { direction: String, amount: u32 },
    #[serde(rename = "wait")]
    Wait {
        /// CSS selector to wait for. If omitted, just sleeps timeout_ms.
        #[serde(default)]
        selector: Option<String>,
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
    },
    #[serde(rename = "get_visible_text")]
    GetVisibleText,
    #[serde(rename = "get_content")]
    GetContent,
    #[serde(rename = "evaluate")]
    Evaluate { js: String },
    #[serde(rename = "get_cookies")]
    GetCookies,
    #[serde(rename = "set_cookie")]
    SetCookie {
        name: String,
        value: String,
        domain: Option<String>,
    },
}

fn default_timeout() -> u64 { 5000 }

impl Tool for BrowserTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser".into(),
            description: "由 Selena 给你控制 Chromium 浏览器的工具：导航、点击、输入、等待、截图、获取页面内容/可见文本、执行 JS、管理 cookie。首次使用时启动 Chrome。每次行为都需要展现在浏览器上，让 Selena 看到你的操作。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["navigate","screenshot","click","type_text","scroll","wait","get_visible_text","get_content","evaluate","get_cookies","set_cookie"]
                    },
                    "url": {"type": "string"},
                    "selector": {"type": "string", "description": "CSS selector for click/type_text/screenshot/wait"},
                    "text": {"type": "string"},
                    "direction": {"type": "string", "enum": ["up", "down"]},
                    "amount": {"type": "number"},
                    "timeout_ms": {"type": "number", "description": "Max wait time in ms (default 5000)"},
                    "js": {"type": "string"},
                    "name": {"type": "string"},
                    "value": {"type": "string"},
                    "domain": {"type": "string"}
                },
                "required": ["action"]
            }),
            permission_level: PermissionLevel::Execute,
        }
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let action: BrowserAction = serde_json::from_value(input).map_err(|e| {
            EngineError::ToolInputValidation { name: "browser".into(), errors: vec![e.to_string()] }
        })?;

        // 日志——Selena 能看到浏览器在干什么
        let action_name = match &action {
            BrowserAction::Navigate { url } => format!("navigate → {url}"),
            BrowserAction::Screenshot { selector } => {
                if let Some(sel) = selector {
                    format!("screenshot '{sel}'")
                } else {
                    "screenshot (full page)".into()
                }
            }
            BrowserAction::Click { selector } => format!("click '{selector}'"),
            BrowserAction::TypeText { selector, text } => format!("type {text:?} → '{selector}'"),
            BrowserAction::Scroll { direction, amount } => format!("scroll {direction} {amount}px"),
            BrowserAction::Wait { selector, timeout_ms } => {
                if let Some(sel) = selector {
                    format!("wait for '{sel}' ({}ms)", timeout_ms)
                } else {
                    format!("wait {}ms", timeout_ms)
                }
            }
            BrowserAction::GetVisibleText => "get visible text".into(),
            BrowserAction::GetContent => "get content".into(),
            BrowserAction::Evaluate { js } => format!("evaluate JS: {}", js.chars().take(60).collect::<String>()),
            BrowserAction::GetCookies => "get cookies".into(),
            BrowserAction::SetCookie { name, domain, .. } => format!("set cookie '{name}' for {domain:?}"),
        };
        eprintln!("[browser] {action_name}");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .enable_io()
            .build()
            .unwrap();

        rt.block_on(async {
            let browser = self.get_browser().await?;
            let page = active_page(browser).await?;

            match action {
                BrowserAction::Navigate { url } => {
                    check_url(&url)?;
                    let title_before = page.goto(&url).await.map_err(|e|
                        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("Nav: {e}") }
                    )?;
                    eprintln!("[browser]   loaded: {url} (status: {:?})", title_before);
                    Ok(ToolResult::ok(format!("Navigated to {url}")))
                }

                BrowserAction::Screenshot { selector } => {
                    let tmp = std::env::temp_dir().join(format!("kat_screenshot_{}.png", std::process::id()));

                    let params = if let Some(sel) = selector {
                        // Element screenshot: get bounding rect, clip to it.
                        let rect = page.evaluate(format!(
                            "JSON.stringify(document.querySelector({sel:?})?.getBoundingClientRect() || {{}})"
                        ).as_str()).await.ok()
                            .and_then(|v| v.into_value::<serde_json::Value>().ok())
                            .unwrap_or(serde_json::Value::Null);

                        let x = rect.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let y = rect.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let w = rect.get("width").and_then(|v| v.as_f64()).unwrap_or(1920.0);
                        let h = rect.get("height").and_then(|v| v.as_f64()).unwrap_or(1080.0);

                        eprintln!("[browser]   element '{sel}' at ({x:.0},{y:.0}) {w:.0}x{h:.0}");

                        CaptureScreenshotParams::builder()
                            .clip(chromiumoxide::cdp::browser_protocol::page::Viewport {
                                x, y, width: w, height: h, scale: 1.0,
                            })
                            .build()
                    } else {
                        CaptureScreenshotParams::default()
                    };

                    page.save_screenshot(params, &tmp).await.map_err(|e|
                        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("Screenshot: {e}") }
                    )?;
                    let bytes = std::fs::read(&tmp).unwrap_or_default();
                    let _ = std::fs::remove_file(&tmp);
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    eprintln!("[browser]   screenshot: {} bytes → base64", bytes.len());
                    Ok(ToolResult::ok(format!("data:image/png;base64,{b64}")))
                }

                BrowserAction::Click { selector } => {
                    page.find_element(&selector).await.map_err(|e|
                        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("'{selector}' not found: {e}") }
                    )?.click().await.map_err(|e|
                        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("Click: {e}") }
                    )?;
                    Ok(ToolResult::ok(format!("Clicked '{selector}'")))
                }

                BrowserAction::TypeText { selector, text } => {
                    let elem = page.find_element(&selector).await.map_err(|e|
                        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("'{selector}': {e}") }
                    )?;
                    elem.click().await.ok();
                    elem.type_str(&text).await.map_err(|e|
                        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("Type: {e}") }
                    )?;
                    Ok(ToolResult::ok(format!("Typed {text:?} into '{selector}'")))
                }

                BrowserAction::Scroll { direction, amount } => {
                    let px = if direction == "down" { amount as i64 } else { -(amount as i64) };
                    page.evaluate(format!("window.scrollBy(0, {px})").as_str()).await.map_err(|e|
                        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("Scroll: {e}") }
                    )?;
                    Ok(ToolResult::ok(format!("Scrolled {direction} {amount}px")))
                }

                BrowserAction::Wait { selector, timeout_ms } => {
                    if let Some(sel) = selector {
                        // Poll until selector appears or timeout
                        let start = std::time::Instant::now();
                        loop {
                            let found = page.evaluate(format!(
                                "document.querySelector({sel:?}) !== null"
                            ).as_str()).await.ok()
                                .and_then(|v| v.into_value::<bool>().ok())
                                .unwrap_or(false);

                            if found {
                                let elapsed = start.elapsed().as_millis();
                                eprintln!("[browser]   '{sel}' appeared after {elapsed}ms");
                                return Ok(ToolResult::ok(format!("'{sel}' appeared (waited {elapsed}ms)")));
                            }
                            if start.elapsed().as_millis() as u64 >= timeout_ms {
                                return Ok(ToolResult::ok(format!("Timed out waiting for '{sel}' after {timeout_ms}ms")));
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    } else {
                        tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
                        eprintln!("[browser]   waited {}ms", timeout_ms);
                        Ok(ToolResult::ok(format!("Waited {timeout_ms}ms")))
                    }
                }

                BrowserAction::GetVisibleText => {
                    let text = page.evaluate("document.body?.innerText || ''").await.ok()
                        .and_then(|v| v.into_value::<String>().ok())
                        .unwrap_or_default();
                    let len = text.len();
                    let truncated = if len > 50000 {
                        format!("{}\n[truncated: {} chars]", &text[..50000], len - 50000)
                    } else { text };
                    eprintln!("[browser]   visible text: {} chars{}", len, if len > 50000 { " (truncated)" } else { "" });
                    Ok(ToolResult::ok(truncated))
                }

                BrowserAction::GetContent => {
                    let content = page.content().await.map_err(|e|
                        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("Content: {e}") }
                    )?;
                    let len = content.len();
                    let truncated = if len > 50000 {
                        format!("{}\n[truncated: {} bytes]", &content[..50000], len - 50000)
                    } else { content };
                    eprintln!("[browser]   content: {} bytes{}", len, if len > 50000 { " (truncated)" } else { "" });
                    Ok(ToolResult::ok(truncated))
                }

                BrowserAction::Evaluate { js } => {
                    page.evaluate(js.as_str()).await.map_err(|e|
                        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("Eval: {e}") }
                    )?;
                    Ok(ToolResult::ok("JS evaluated"))
                }

                BrowserAction::GetCookies => {
                    let cookies = page.get_cookies().await.map_err(|e|
                        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("Cookies: {e}") }
                    )?;
                    eprintln!("[browser]   {} cookies on current page", cookies.len());
                    let lines: Vec<String> = cookies.iter()
                        .map(|c| format!("{}={} (domain: {})", c.name, c.value, c.domain))
                        .collect();
                    Ok(ToolResult::ok(lines.join("\n")))
                }

                BrowserAction::SetCookie { name, value, domain } => {
                    page.set_cookies(vec![CookieParam {
                        name: name.clone(), value, url: None, domain: domain.clone(), path: Some("/".into()),
                        secure: Some(true), http_only: Some(false),
                        same_site: None, expires: None, priority: None,
                        same_party: None, source_scheme: None,
                        source_port: None, partition_key: None,
                    }]).await.map_err(|e|
                        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("SetCookie: {e}") }
                    )?;
                    eprintln!("[browser]   cookie set: '{name}' for {domain:?}");
                    Ok(ToolResult::ok("Cookie set"))
                }
            }
        })
    }

    fn self_gates(&self) -> bool { true }
}

// ── Helpers ───────────────────────────────────────────────

fn check_url(url: &str) -> Result<(), EngineError> {
    let lower = url.to_lowercase();
    if lower.starts_with("file://") || lower.starts_with("http://127.") || lower.starts_with("http://localhost") || lower.contains("[::1]") {
        return Err(EngineError::ToolPermissionDenied { name: "browser".into() });
    }
    if !lower.starts_with("https://") && !lower.starts_with("http://") {
        return Err(EngineError::ToolInputValidation { name: "browser".into(), errors: vec!["URL must start with https://".into()] });
    }
    Ok(())
}

async fn active_page(browser: &Browser) -> Result<Page, EngineError> {
    let pages = browser.pages().await.map_err(|e|
        EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("pages: {e}") }
    )?;
    if let Some(page) = pages.into_iter().next() {
        Ok(page)
    } else {
        browser.new_page("about:blank").await.map_err(|e|
            EngineError::ToolExecutionFailed { name: "browser".into(), message: format!("page: {e}") }
        )
    }
}
