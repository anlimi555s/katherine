// mcp_http.rs — Katherine MCP HTTP 服务器。
// 和现有 mcp-http.ts 协议对齐：/health, /chat/stream, /permission。
// VS Code 扩展通过此端口通信。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use katherine_core::event::AgentEvent;
use katherine_core::neuro::Neuro;
use katherine_core::types::Message;
use katherine_engine::loop_::{run_loop, LoopConfig};
use katherine_engine::neuro_impl::MemNeuro;
use katherine_engine::providers::deepseek::DeepSeekProvider;
use katherine_engine::tools::ToolRegistry;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::wake;

// ── MCP HTTP Server ──────────────────────────────────────

/// 权限挂起请求。
struct PendingPermission {
    resolve: tokio::sync::oneshot::Sender<bool>,
}

/// 启动 MCP HTTP 服务器。
pub async fn serve(port: u16) {
    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to bind port {port}: {e}");
            std::process::exit(1);
        });

    let identity = wake::assemble();
    let db_path = std::env::var("KATHERINE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("katherine-memories")
        .join("katherine.db");
    let hub: Arc<dyn katherine_core::hub::Hub> =
        match katherine_engine::memory::libsql_store::LibSqlHub::new(db_path) {
            Ok(store) => Arc::new(store),
            Err(e) => {
                eprintln!("FATAL: Cannot open libSQL: {e}");
                std::process::exit(1);
            }
        };
    let provider: Arc<dyn katherine_core::provider::LlmProvider> =
        match DeepSeekProvider::from_env() {
            Ok(p) => Arc::new(p),
            Err(e) => {
                eprintln!("Warning: {e}");
                eprintln!("Set ANTHROPIC_AUTH_TOKEN to use DeepSeek.");
                return;
            }
        };
    let neuro = Arc::new(MemNeuro::new());
    let tools = Arc::new(ToolRegistry::with_defaults(hub.clone(), neuro.clone()));

    // LocalMemoryStore 永不断连——健康检查仅对 HttpHub 有意义
    neuro.set_hub_connected(true);
    eprintln!("Memory: local store (katherine-memories/memory.json)");

    let permissions: Arc<Mutex<HashMap<String, PendingPermission>>> =
        Arc::new(Mutex::new(HashMap::new()));

    eprintln!("Katherine MCP HTTP server on http://127.0.0.1:{port}");
    eprintln!("{} tools registered", tools.definitions().len());

    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Accept error: {e}");
                continue;
            }
        };

        let identity = identity.clone();
        let provider = provider.clone();
        let tools = tools.clone();
        let neuro = neuro.clone();
        let permissions = permissions.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_request(
                stream,
                identity,
                provider,
                tools,
                neuro,
                permissions,
            )
            .await
            {
                eprintln!("Request error from {addr}: {e}");
            }
        });
    }
}

async fn handle_request(
    mut stream: TcpStream,
    identity: String,
    provider: Arc<dyn katherine_core::provider::LlmProvider>,
    tools: Arc<ToolRegistry>,
    neuro: Arc<MemNeuro>,
    permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 读取 HTTP 请求头
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    let first_line = request.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let method = parts[0];
    let path = parts[1];

    // 解析 body（如果有）
    let body_start = request.find("\r\n\r\n").map(|i| i + 4).unwrap_or(request.len());
    let body = if body_start < n {
        &buf[body_start..n]
    } else {
        &[]
    };

    let cors = "Access-Control-Allow-Origin: *\r\n";

    if method == "OPTIONS" {
        let resp = format!(
            "HTTP/1.1 200 OK\r\n{cors}Access-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nContent-Length: 0\r\n\r\n"
        );
        stream.write_all(resp.as_bytes()).await?;
        return Ok(());
    }

    match (method, path) {
        ("GET", p) if p.starts_with("/health") || p == "/noop" => {
            let tools = tools.definitions();
            let json = serde_json::json!({
                "status": "ok",
                "tools": tools.len()
            });
            let body = serde_json::to_string(&json)?;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{cors}Content-Length: {}\r\n\r\n{}",
                body.len(), body
            );
            stream.write_all(resp.as_bytes()).await?;
        }

        ("GET", p) if p.starts_with("/status") => {
            let status = neuro.status();
            let json = serde_json::json!(status);
            let body = serde_json::to_string(&json)?;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{cors}Content-Length: {}\r\n\r\n{}",
                body.len(), body
            );
            stream.write_all(resp.as_bytes()).await?;
        }

        // 前端页面
        ("GET", p) if p == "/" || p == "/index.html" => {
            let html_path = std::env::var("KATHERINE_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join("katherine-memories")
                .join("katherine.html");
            match tokio::fs::read_to_string(&html_path).await {
                Ok(html) => {
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n{cors}Content-Length: {}\r\n\r\n{}",
                        html.len(), html
                    );
                    stream.write_all(resp.as_bytes()).await?;
                }
                Err(_) => {
                    let msg = "404";
                    let resp = format!(
                        "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\n\r\n{}",
                        msg.len(), msg
                    );
                    stream.write_all(resp.as_bytes()).await?;
                }
            }
        }

        ("POST", p) if p.starts_with("/chat") && !p.contains("stream") => {
            let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
            let prompt = json["prompt"].as_str().unwrap_or("");
            let history: Vec<Message> = json
                .get("history")
                .and_then(|h| h.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| {
                            let role = if m["role"].as_str()? == "assistant" {
                                katherine_core::types::Role::Assistant
                            } else {
                                katherine_core::types::Role::User
                            };
                            let content = parse_history_content(&m["content"]);
                            if content.is_empty() { return None; }
                            Some(Message { role, content })
                        })
                        .collect()
                })
                .unwrap_or_default();

            let mut messages = history;
            messages.push(Message::user(prompt));

            let cancel = CancellationToken::new();
            let ev_stream = run_loop(
                provider.clone(),
                tools.clone(),
                Arc::new(katherine_engine::memory::libsql_store::LibSqlHub::new(std::path::PathBuf::from("katherine-memories/katherine.db")).expect("libSQL")),
                neuro.clone(),
                messages,
                identity.clone(),
                LoopConfig {
                    max_turns: 5,
                    ..LoopConfig::default()
                },
                cancel,
            );

            tokio::pin!(ev_stream);
            let mut reply = String::new();
            let mut tool_calls: Vec<serde_json::Value> = Vec::new();
            while let Some(ev) = ev_stream.next().await {
                match ev {
                    AgentEvent::Text { text } => reply.push_str(&text),
                    AgentEvent::ToolCall { name, input, .. } => {
                        tool_calls.push(serde_json::json!({"name": name, "input": input, "result": null}));
                    }
                    AgentEvent::ToolResult { tool_use_id: _, result } => {
                        if let Some(tc) = tool_calls.iter_mut().rev().find(|tc| tc["result"].is_null()) {
                            tc["result"] = serde_json::json!(result.content);
                        }
                    }
                    AgentEvent::Done { .. } => break,
                    _ => {}
                }
            }

            let resp_json = if tool_calls.is_empty() {
                serde_json::json!({"reply": reply})
            } else {
                serde_json::json!({"reply": reply, "toolCalls": tool_calls})
            };
            let body = serde_json::to_string(&resp_json)?;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{cors}Content-Length: {}\r\n\r\n{}",
                body.len(), body
            );
            stream.write_all(resp.as_bytes()).await?;
        }

        ("POST", p) if p.starts_with("/chat/stream") => {
            let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
            let prompt = json["prompt"].as_str().unwrap_or("");
            let history: Vec<Message> = json
                .get("history")
                .and_then(|h| h.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| {
                            let role = if m["role"].as_str()? == "assistant" {
                                katherine_core::types::Role::Assistant
                            } else {
                                katherine_core::types::Role::User
                            };
                            let content = parse_history_content(&m["content"]);
                            if content.is_empty() { return None; }
                            Some(Message { role, content })
                        })
                        .collect()
                })
                .unwrap_or_default();

            let mut messages = history;
            messages.push(Message::user(prompt));

            // 权限桥接（Phase 7: 接入 run_loop 的 permission_callback）
            let _permissions = permissions.clone();

            let cancel = CancellationToken::new();

            // SSE 响应头
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n{cors}\r\n"
            );
            stream.write_all(header.as_bytes()).await?;

            // 运行 loop + stream events
            let event_stream = run_loop(
                provider,
                tools,
                Arc::new(katherine_engine::memory::libsql_store::LibSqlHub::new(std::path::PathBuf::from("katherine-memories/katherine.db")).expect("libSQL")),
                neuro,
                messages,
                identity,
                LoopConfig {
                    max_turns: 10,
                    ..LoopConfig::default()
                },
                cancel,
            );

            tokio::pin!(event_stream);
            let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                tokio::select! {
                    _ = heartbeat.tick() => {
                        // SSE keep-alive——防浏览器/proxy 空闲断连
                        if stream.write_all(b": heartbeat\n\n").await.is_err() {
                            break;
                        }
                    }
                    ev = event_stream.next() => {
                        match ev {
                            Some(event) => {
                                let json = serde_json::to_string(&event).unwrap_or_default();
                                let sse = format!("data: {}\n\n", json);
                                if stream.write_all(sse.as_bytes()).await.is_err() {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
            let _ = stream.write_all(b"data: [DONE]\n\n").await;
        }

        ("POST", p) if p.starts_with("/permission") => {
            let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
            let id = json["id"].as_str().unwrap_or("");
            let decision = json["decision"].as_str().unwrap_or("deny");

            let allow = decision == "allow";
            if let Some(entry) = permissions.lock().unwrap().remove(id) {
                let _ = entry.resolve.send(allow);
            }

            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{cors}Content-Length: 17\r\n\r\n{{\"status\":\"ok\"}}"
            );
            stream.write_all(resp.as_bytes()).await?;
        }

        _ => {
            let body = "Katherine MCP HTTP Server";
            let resp = format!(
                "HTTP/1.1 200 OK\r\n{cors}Content-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).await?;
        }
    }

    Ok(())
}

/// 解析前端传来的 history 消息中的 content 字段。
/// 兼容两种格式：字符串 "hello" 和数组 [{"type":"text","text":"hello"}, ...]
fn parse_history_content(content_val: &serde_json::Value) -> Vec<katherine_core::types::ContentBlock> {
    use katherine_core::types::ContentBlock;
    // 字符串格式
    if let Some(s) = content_val.as_str() {
        if s.is_empty() { return vec![]; }
        return vec![ContentBlock::Text { text: s.to_string() }];
    }
    // 数组格式
    if let Some(arr) = content_val.as_array() {
        return arr.iter().filter_map(|b| {
            match b["type"].as_str()? {
                "text" => Some(ContentBlock::Text {
                    text: b["text"].as_str().unwrap_or("").to_string(),
                }),
                "thinking" => Some(ContentBlock::Thinking {
                    thinking: b["thinking"].as_str().unwrap_or("").to_string(),
                    signature: b["signature"].as_str().unwrap_or("").to_string(),
                }),
                "tool_use" => Some(ContentBlock::ToolUse {
                    id: b["id"].as_str().unwrap_or("").to_string(),
                    name: b["name"].as_str().unwrap_or("").to_string(),
                    input: b.get("input").cloned().unwrap_or(serde_json::Value::Null),
                }),
                "tool_result" => Some(ContentBlock::ToolResult {
                    tool_use_id: b["tool_use_id"].as_str().unwrap_or("").to_string(),
                    content: b["content"].as_str().unwrap_or("").to_string(),
                    is_error: b["is_error"].as_bool().unwrap_or(false),
                }),
                _ => None,
            }
        }).collect();
    }
    vec![]
}
