// providers/deepseek.rs — DeepSeek Provider。
// 通过 DeepSeek 的 Anthropic-compatible API 调用模型。
// 手动 SSE 解析——不依赖 Anthropic SDK。

use std::path::PathBuf;
use std::pin::Pin;

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
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .map_err(|e| EngineError::Config(format!("Failed to build HTTP client: {e}")))?,
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
        let is_streaming = request.stream;
        let url = format!("{}/v1/messages", self.base_url);
        let api_key = self.api_key.clone();
        let client = self.client.clone();

        Box::pin(async_stream::stream! {
            eprintln!("[provider] POST {} (stream={})", url, is_streaming);

            if !is_streaming {
                // ── 非流式：读完整 JSON 响应 ──────────
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
                        yield Err(EngineError::ProviderStreamInterrupted(format!(
                            "HTTP request failed: {e}"
                        )));
                        return;
                    }
                };

                let status = response.status();
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    yield Err(EngineError::ProviderAuth("Invalid API key".into()));
                    return;
                }
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(5);
                    yield Err(EngineError::ProviderRateLimited { retry_after_s: retry_after });
                    return;
                }
                if status == reqwest::StatusCode::from_u16(529).unwrap() {
                    yield Err(EngineError::ProviderOverloaded);
                    return;
                }
                if !status.is_success() {
                    let code = status.as_u16();
                    let msg = response.text().await.unwrap_or_default();
                    eprintln!("[provider] error {code}: {msg}");
                    yield Err(EngineError::ProviderServer(code, msg));
                    return;
                }

                let full: serde_json::Value = match response.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        yield Err(EngineError::ProviderStreamInterrupted(format!(
                            "Failed to parse JSON response: {e}"
                        )));
                        return;
                    }
                };

                eprintln!("[provider] non-streaming response received");

                if let Some(content) = full.get("content").and_then(|c| c.as_array()) {
                    for block in content {
                        match block.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    yield Ok(StreamEvent::TextDelta { text: text.to_string() });
                                }
                            }
                            Some("tool_use") => {
                                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("?").to_string();
                                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?").to_string();
                                yield Ok(StreamEvent::ToolUseStart { id: id.clone(), name: name.clone() });
                                yield Ok(StreamEvent::ToolUseEnd);
                            }
                            _ => {}
                        }
                    }
                }
                yield Ok(StreamEvent::MessageStop);
                return;
            }

            // ── 流式：SSE 解析（原有逻辑）──────────────
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
                    yield Err(EngineError::ProviderStreamInterrupted(format!(
                        "HTTP request failed: {e}"
                    )));
                    return;
                }
            };

            let status = response.status();
            let status_code = status.as_u16();
            eprintln!("[provider] streaming (status {status_code})");
            if status == reqwest::StatusCode::UNAUTHORIZED {
                yield Err(EngineError::ProviderAuth("Invalid API key".into()));
                return;
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(5);
                eprintln!("[provider] 429 rate limited, retry-after={retry_after}s");
                yield Err(EngineError::ProviderRateLimited { retry_after_s: retry_after });
                return;
            }
            if status == reqwest::StatusCode::from_u16(529).unwrap() {
                eprintln!("[provider] 529 overloaded");
                yield Err(EngineError::ProviderOverloaded);
                return;
            }
            if !status.is_success() {
                let msg = response.text().await.unwrap_or_default();
                eprintln!("[provider] error {status_code}: {msg}");
                yield Err(EngineError::ProviderServer(status_code, msg));
                return;
            }

            // 双超时窗口 (Eloquent + free-code):
            //   TTFT: 30s — 首 token/事件前。快速失败、快速重试
            //   inter-token: 90s — 流中卡死检测。匹配 free-code STREAM_IDLE_TIMEOUT_MS
            let mut got_first_content = false;

            // 流式读取 SSE
            let byte_stream = response.bytes_stream();
            tokio::pin!(byte_stream);

            let mut buffer = String::new();
            let mut event_type = String::new();
            let mut event_data = String::new();
            let mut current_block: Option<String> = None; // "thinking" | "tool_use" | "text"

            loop {
                let timeout_ms = if got_first_content { 90_000 } else { 30_000 };
                let chunk_result = match tokio::time::timeout(
                    std::time::Duration::from_millis(timeout_ms),
                    byte_stream.next()
                ).await {
                    Ok(Some(result)) => result,
                    Ok(None) => break, // stream ended
                    Err(_elapsed) => {
                        // 超时：TTFT 或 inter-token
                        let label = if got_first_content { "Inter-token timeout (90s)" } else { "TTFT timeout (30s)" };
                        yield Err(EngineError::ProviderStreamInterrupted(label.into()));
                        return;
                    }
                };

                let bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        if buffer.is_empty() && event_data.is_empty() {
                            yield Err(EngineError::ProviderStreamInterrupted(format!(
                                "Stream read error: {e}"
                            )));
                            return;
                        }
                        break;
                    }
                };

                got_first_content = true;
                buffer.push_str(&String::from_utf8_lossy(&bytes));

                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim().to_string();
                    buffer = buffer[line_end + 1..].to_string();

                    if line.is_empty() {
                        if event_type == "message_stop" {
                        }
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
                                Some(Err(e)) => yield Err(e),
                                None => {}
                            }
                        }
                        event_type.clear();
                        event_data.clear();
                        continue;
                    }

                    if line.starts_with(':') {
                        // SSE comment / keep-alive（DeepSeek 高负载心跳）
                        yield Ok(StreamEvent::Heartbeat);
                    } else if let Some(ev) = line.strip_prefix("event: ") {
                        event_type = ev.trim().to_string();
                    } else if let Some(data) = line.strip_prefix("data: ") {
                        event_data = data.trim().to_string();
                    }
                }
            }
        })
    }
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
