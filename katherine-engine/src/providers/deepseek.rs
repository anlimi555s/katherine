// providers/deepseek.rs — DeepSeek Provider。
// 通过 DeepSeek 的 Anthropic-compatible API 调用模型。
// 手动 SSE 解析——不依赖 Anthropic SDK。

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::Stream;
use katherine_core::error::EngineError;
use katherine_core::event::StreamEvent;
use katherine_core::provider::{LlmProvider, Request};
use katherine_core::types::{ContentBlock, Role};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use futures::StreamExt;

/// DeepSeek 的 Anthropic 兼容端点。
const DEFAULT_BASE_URL: &str = "https://api.deepseek.com/anthropic";
const DEFAULT_MODEL: &str = "deepseek-v4-pro[1m]";

// ── 重试配置 ──────────────────────────────────────
/// 最大重试次数（不含首次）。
const MAX_RETRIES: u32 = 4;
/// 退避基数 ms。
const BASE_DELAY_MS: u64 = 2000;
/// 退避上限 ms。
const MAX_DELAY_MS: u64 = 32_000;
/// 连续失败次数阈值，超过则熔断。
const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;
/// 熔断冷却时间 s。
const CIRCUIT_COOLDOWN_SECS: u64 = 30;

/// 断路器状态。
#[derive(Debug)]
struct CircuitBreaker {
    consecutive_failures: u32,
    is_open: bool,
    opened_at: Option<Instant>,
}

impl CircuitBreaker {
    fn new() -> Self {
        CircuitBreaker {
            consecutive_failures: 0,
            is_open: false,
            opened_at: None,
        }
    }

    /// 检查是否允许调用。熔断打开时检查冷却是否到期。
    fn allow(&mut self) -> bool {
        if !self.is_open {
            return true;
        }
        if let Some(at) = self.opened_at {
            if at.elapsed().as_secs() >= CIRCUIT_COOLDOWN_SECS {
                // 半开——放行一次探针
                self.is_open = false;
                self.consecutive_failures = 0;
                return true;
            }
        }
        false
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.is_open = false;
        self.opened_at = None;
    }

    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= CIRCUIT_BREAKER_THRESHOLD {
            self.is_open = true;
            self.opened_at = Some(Instant::now());
        }
    }
}

/// 配置——优先从 config.json 读，环境变量做 fallback。
#[derive(Debug, Deserialize)]
struct ApiConfig {
    api: Option<ApiSection>,
}

#[derive(Debug, Deserialize)]
struct ApiSection {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
}

/// DeepSeek API provider。
pub struct DeepSeekProvider {
    client: Client,
    circuit_breaker: Arc<Mutex<CircuitBreaker>>,
    base_url: String,
    model: String,
    api_key: String,
}

impl DeepSeekProvider {
    /// 创建 provider——先读 config.json，再读环境变量。
    pub fn from_config() -> Result<Self, EngineError> {
        let (base_url, api_key, model) = Self::load_config();

        let key_prefix: String = api_key.chars().take(10).collect();
        eprintln!("[provider] key_prefix={key_prefix}... base_url={base_url} model={model}");

        Ok(DeepSeekProvider {
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .read_timeout(std::time::Duration::from_secs(120))
                .build()
                .map_err(|e| EngineError::Config(format!("Failed to build HTTP client: {e}")))?,
            circuit_breaker: Arc::new(Mutex::new(CircuitBreaker::new())),
            base_url,
            model,
            api_key,
        })
    }

    /// 保留旧接口兼容性。
    pub fn from_env() -> Result<Self, EngineError> {
        Self::from_config()
    }

    fn load_config() -> (String, String, String) {
        // 1. 找 config.json
        if let Some(cfg) = Self::find_config() {
            if let Some(api) = cfg.api {
                let base_url = api.base_url
                    .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
                    .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
                let api_key = api.api_key
                    .or_else(|| std::env::var("ANTHROPIC_AUTH_TOKEN").ok());
                let model = api.model
                    .or_else(|| std::env::var("ANTHROPIC_MODEL").ok())
                    .unwrap_or_else(|| DEFAULT_MODEL.to_string());
                if let Some(key) = api_key {
                    return (base_url, key, model);
                }
            }
        }

        // 2. fallback——纯环境变量
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let api_key = std::env::var("ANTHROPIC_AUTH_TOKEN")
            .unwrap_or_else(|_| "".into());
        let model = std::env::var("ANTHROPIC_MODEL")
            .unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        (base_url, api_key, model)
    }

    fn find_config() -> Option<ApiConfig> {
        let candidate_dirs: Vec<PathBuf> = std::env::var("KATHERINE_HOME")
            .ok()
            .map(|h| vec![PathBuf::from(&h).join("katherine-memories")])
            .unwrap_or_default()
            .into_iter()
            .chain(std::env::current_dir().ok().into_iter().flat_map(|cwd| {
                cwd.ancestors()
                    .map(|a| a.join("katherine-memories"))
                    .collect::<Vec<_>>()
            }))
            .collect();

        for dir in &candidate_dirs {
            let path = dir.join("config.json");
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(cfg) = serde_json::from_str::<ApiConfig>(&raw) {
                    eprintln!("[provider] loaded config from {}", path.display());
                    return Some(cfg);
                }
            }
        }
        None
    }

    /// 构造 Anthropic-compatible 请求体。
    fn build_body(&self, request: &Request) -> Value {
        let messages: Vec<Value> = request
            .messages
            .iter()
            .filter(|m| m.role != Role::Assistant || !m.content.is_empty())
            .map(|m| {
                let role = match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                let content: Vec<Value> = m
                    .content
                    .iter()
                    .map(|block| match block {
                        ContentBlock::Text { text } => {
                            serde_json::json!({"type": "text", "text": text})
                        }
                        ContentBlock::Thinking { thinking, signature } => {
                            serde_json::json!({"type": "thinking", "thinking": thinking, "signature": signature})
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            serde_json::json!({
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": input,
                            })
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": tool_use_id,
                                "content": content,
                                "is_error": is_error,
                            })
                        }
                    })
                    .collect();
                serde_json::json!({"role": role, "content": content})
            })
            .collect();

        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| t.to_api_format())
            .collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": request.max_tokens,
            "stream": request.stream,
            "system": request.system_prompt,
            "messages": messages,
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools);
        }

        body
    }
}

impl LlmProvider for DeepSeekProvider {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn stream(
        &self,
        request: Request,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, EngineError>> + Send + '_>> {
        let body = self.build_body(&request);
        let original_streaming = request.stream;
        let url = format!("{}/v1/messages", self.base_url);
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let breaker = Arc::clone(&self.circuit_breaker);

        Box::pin(async_stream::stream! {
            let mut attempt: u32 = 0;

            loop {
                attempt += 1;

                // ── 断路检查（锁提前释放，不跨 await）─
                let allowed = {
                    let mut cb = breaker.lock().unwrap();
                    cb.allow()
                };
                if !allowed {
                    yield Err(EngineError::ProviderServer(503,
                        "Circuit breaker open — too many consecutive failures".into()));
                    return;
                }

                // 首次走流式，重试走非流式
                let use_stream = original_streaming && attempt == 1;
                eprintln!("[provider] POST {url} (stream={use_stream}, attempt={attempt})");

                // ── HTTP 请求（流式与非流式共享）──────
                let response = match client
                    .post(&url)
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", "2023-06-01")
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        let err = EngineError::ProviderStreamInterrupted(format!(
                            "HTTP request failed: {e}"
                        ));
                        if err.is_transient() && attempt <= MAX_RETRIES + 1 {
                            breaker.lock().unwrap().record_failure();
sleep_backoff(attempt).await;
                            continue;
                        }
                        yield Err(err);
                        return;
                    }
                };

                // ── 状态码处理 ──────────────────────
                let status = response.status();
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    // 401 — 不可重试，不更新断路器
                    yield Err(EngineError::ProviderAuth("Invalid API key".into()));
                    return;
                }
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    let retry_after: u64 = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(5);
                    let err = EngineError::ProviderRateLimited { retry_after_s: retry_after };
                    if attempt <= MAX_RETRIES + 1 {
                        breaker.lock().unwrap().record_failure();
eprintln!("[provider] 429 rate limited, retry-after={retry_after}s");
                        tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
                        continue;
                    }
                    yield Err(err);
                    return;
                }
                if status == reqwest::StatusCode::from_u16(529).unwrap() {
                    let err = EngineError::ProviderOverloaded;
                    if attempt <= MAX_RETRIES + 1 {
                        breaker.lock().unwrap().record_failure();
eprintln!("[provider] 529 overloaded, attempt {attempt}");
                        sleep_backoff(attempt).await;
                        continue;
                    }
                    yield Err(err);
                    return;
                }
                if !status.is_success() {
                    let code = status.as_u16();
                    let msg = response.text().await.unwrap_or_default();
                    let err = EngineError::ProviderServer(code, msg);
                    if err.is_transient() && attempt <= MAX_RETRIES + 1 {
                        breaker.lock().unwrap().record_failure();
                        eprintln!("[provider] error {code}, retry attempt {attempt}");
                        sleep_backoff(attempt).await;
                        continue;
                    }
                    yield Err(err);
                    return;
                }

                // ── 成功 — 复位断路器 ──────────────
                breaker.lock().unwrap().record_success();

                // ── 非流式：解析 JSON 响应体 ──────────
                if !use_stream {
                    let full: serde_json::Value = match response.json().await {
                        Ok(v) => v,
                        Err(e) => {
                            let err = EngineError::ProviderStreamInterrupted(format!(
                                "Failed to parse JSON response: {e}"
                            ));
                            if attempt <= MAX_RETRIES + 1 {
                                breaker.lock().unwrap().record_failure();
        sleep_backoff(attempt).await;
                                continue;
                            }
                            yield Err(err);
                            return;
                        }
                    };

                    eprintln!("[provider] non-streaming response received");

                    let mut got_content = false;
                    if let Some(content) = full.get("content").and_then(|c| c.as_array()) {
                        for block in content {
                            match block.get("type").and_then(|t| t.as_str()) {
                                Some("text") => {
                                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                        got_content = true;
                                        yield Ok(StreamEvent::TextDelta { text: text.to_string() });
                                    }
                                }
                                Some("tool_use") => {
                                    let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("?").to_string();
                                    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?").to_string();
                                    got_content = true;
                                    yield Ok(StreamEvent::ToolUseStart { id: id.clone(), name: name.clone() });
                                    yield Ok(StreamEvent::ToolUseEnd);
                                }
                                _ => {}
                            }
                        }
                    }
                    // 空响应 — 如果是 transient 则重试
                    if !got_content && attempt <= MAX_RETRIES + 1 {
                        breaker.lock().unwrap().record_failure();
                        eprintln!("[provider] empty non-streaming response, retry attempt {attempt}");
                        sleep_backoff(attempt).await;
                        continue;
                    }
                    yield Ok(StreamEvent::MessageStop);
                    return;
                }

                // ── 流式：SSE 解析 ─────────────────
                // 双超时窗口 (Eloquent + free-code):
                //   TTFT: 30s — 首 token/事件前。快速失败、快速重试
                //   inter-token: 90s — 流中卡死检测
                let status_code = status.as_u16();
                eprintln!("[provider] streaming (status {status_code})");

                let mut got_first_content = false;
                let mut got_any_content = false;

                let byte_stream = response.bytes_stream();
                tokio::pin!(byte_stream);

                let mut buffer = String::new();
                let mut event_type = String::new();
                let mut event_data = String::new();
                let mut current_block: Option<String> = None;

                loop {
                    let timeout_ms = if got_first_content { 90_000 } else { 30_000 };
                    let chunk_result = match tokio::time::timeout(
                        std::time::Duration::from_millis(timeout_ms),
                        byte_stream.next()
                    ).await {
                        Ok(Some(result)) => result,
                        Ok(None) => break,
                        Err(_elapsed) => {
                            let label = if got_first_content {
                                "Inter-token timeout (90s)"
                            } else {
                                "TTFT timeout (30s)"
                            };
                            let err = EngineError::ProviderStreamInterrupted(label.into());
                            // 有内容 → 不重试（方案 A），用已收到的
                            if got_any_content {
                                eprintln!("[provider] stream timeout after content, using partial");
                                return;
                            }
                            // 无内容 + transient → 重试
                            if attempt <= MAX_RETRIES + 1 {
                                breaker.lock().unwrap().record_failure();
        sleep_backoff(attempt).await;
                                break; // 跳出 SSE 循环，进入外层 retry loop
                            }
                            yield Err(err);
                            return;
                        }
                    };

                    let bytes = match chunk_result {
                        Ok(b) => b,
                        Err(e) => {
                            if buffer.is_empty() && event_data.is_empty() {
                                let err = EngineError::ProviderStreamInterrupted(format!(
                                    "Stream read error: {e}"
                                ));
                                if !got_any_content && attempt <= MAX_RETRIES + 1 {
                                    breaker.lock().unwrap().record_failure();
                sleep_backoff(attempt).await;
                                    break;
                                }
                                yield Err(err);
                                return;
                            }
                            // 缓冲区有部分数据，处理完就走
                            break;
                        }
                    };

                    got_first_content = true;
                    got_any_content = true;
                    buffer.push_str(&String::from_utf8_lossy(&bytes));

                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim().to_string();
                        buffer = buffer[line_end + 1..].to_string();

                        if line.is_empty() {
                            if !event_data.is_empty() {
                                match parse_sse_data(&event_type, &event_data, &mut current_block) {
                                    Some(Ok(event)) => {
                                        let is_message_stop =
                                            matches!(event, StreamEvent::MessageStop);
                                        yield Ok(event);
                                        if is_message_stop {
                                            return;
                                        }
                                    }
                                    Some(Err(e)) => {
                                        // Mid-stream error — 有内容就不重试
                                        if !got_any_content && e.is_transient()
                                            && attempt <= MAX_RETRIES + 1
                                        {
                                            breaker.lock().unwrap().record_failure();
                                            sleep_backoff(attempt).await;
                                            break;
                                        }
                                        yield Err(e);
                                        return;
                                    }
                                    None => {}
                                }
                            }
                            event_type.clear();
                            event_data.clear();
                            continue;
                        }

                        if line.starts_with(':') {
                            yield Ok(StreamEvent::Heartbeat);
                        } else if let Some(ev) = line.strip_prefix("event: ") {
                            event_type = ev.trim().to_string();
                        } else if let Some(data) = line.strip_prefix("data: ") {
                            event_data = data.trim().to_string();
                        }
                    }
                }
                // SSE 循环结束——如果走到这里是因为 break（触发重试），继续外层循环
                // 如果是正常 break（Ok(None)），流自然结束但没收到 message_stop
                if !got_any_content && attempt <= MAX_RETRIES + 1 {
                    breaker.lock().unwrap().record_failure();
                    sleep_backoff(attempt).await;
                    continue;
                }
                // 有内容就结束——方案 A：不重试部分内容
                return;
            }
        })
    }
}

/// 指数退避 + jitter。
async fn sleep_backoff(attempt: u32) {
    let base = BASE_DELAY_MS.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)));
    let capped = base.min(MAX_DELAY_MS);
    // 0~25% jitter
    let pseudo_rand = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64 % 250;
    let jitter_ms = capped + (capped * pseudo_rand / 1000);
    eprintln!("[provider] backoff {jitter_ms}ms (attempt {attempt})");
    tokio::time::sleep(std::time::Duration::from_millis(jitter_ms)).await;
}

/// 解析 SSE 事件数据为 StreamEvent。
fn parse_sse_data(
    event_type: &str,
    data: &str,
    current_block: &mut Option<String>,
) -> Option<Result<StreamEvent, EngineError>> {
    match event_type {
        "content_block_start" => {
            let v: Value = serde_json::from_str(data).ok()?;
            let block = v.get("content_block")?;
            let block_type = block.get("type")?.as_str()?.to_string();
            *current_block = Some(block_type.clone());
            match block_type.as_str() {
                "tool_use" => Some(Ok(StreamEvent::ToolUseStart {
                    id: block.get("id")?.as_str()?.to_string(),
                    name: block.get("name")?.as_str()?.to_string(),
                })),
                "thinking" => {
                    let sig = block.get("signature").and_then(|s| s.as_str()).unwrap_or("").to_string();
                    Some(Ok(StreamEvent::ThinkingStart { signature: sig }))
                }
                _ => None, // text block start — text comes in deltas
            }
        }
        "content_block_delta" => {
            let v: Value = serde_json::from_str(data).ok()?;
            let delta = v.get("delta")?;
            match delta.get("type")?.as_str()? {
                "text_delta" => Some(Ok(StreamEvent::TextDelta {
                    text: delta.get("text")?.as_str()?.to_string(),
                })),
                "thinking_delta" => Some(Ok(StreamEvent::ThinkingDelta {
                    thinking: delta.get("thinking").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                })),
                "input_json_delta" => Some(Ok(StreamEvent::ToolUseDelta {
                    input_json: delta.get("partial_json")?.as_str()?.to_string(),
                })),
                _ => None,
            }
        }
        "content_block_stop" => {
            match current_block.as_deref() {
                Some("thinking") => {
                    *current_block = None;
                    Some(Ok(StreamEvent::ThinkingEnd))
                }
                Some("tool_use") => {
                    *current_block = None;
                    Some(Ok(StreamEvent::ToolUseEnd))
                }
                _ => None,
            }
        }
        "message_delta" => {
            // stop_reason — 内部使用，不产出事件
            None
        }
        "message_stop" => Some(Ok(StreamEvent::MessageStop)),
        _ => None,
    }
}
