// main.rs — Katherine CLI 入口。
// 三种模式：单次（katherine "prompt"）、REPL、MCP HTTP 服务。

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use futures::StreamExt;
use katherine_core::event::AgentEvent;
use katherine_core::types::Message;
use katherine_engine::loop_::{run_loop, LoopConfig};
use katherine_engine::neuro_impl::MemNeuro;
use katherine_engine::persistence::session::{
    JsonlSessionStore, SessionEntry, SessionStore,
};
use katherine_engine::providers::deepseek::DeepSeekProvider;
use katherine_engine::tools::ToolRegistry;
use tokio_util::sync::CancellationToken;

mod identity;
mod mcp_http;
mod wake;
mod watchdog;

#[derive(Parser)]
#[command(name = "katherine", version = "0.1", about = "Katherine Engine")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// 单次 prompt（无子命令时）
    prompt: Option<String>,

    /// 最大轮次数
    #[arg(short = 'n', long, default_value = "10")]
    max_turns: u32,

    /// 模型
    #[arg(short, long, default_value = "deepseek-v4-pro[1m]")]
    model: String,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动 MCP HTTP 服务器（VS Code 扩展连接模式）
    Serve {
        #[arg(short, long, default_value = "9876")]
        port: u16,
        /// 同时启动 watchdog 监控
        #[arg(long)]
        watchdog: bool,
    },
    /// 启动独立 watchdog——监控指定 PID 的引擎进程
    Watch {
        /// 引擎进程 PID
        #[arg(long)]
        pid: u32,
        /// 引擎端口
        #[arg(short, long, default_value = "9876")]
        port: u16,
    },
}

#[tokio::main]
async fn main() {
    // 编译时 identity hash 校验
    let expected = env!("KATHERINE_IDENTITY_HASH");
    if expected != "no_identity_file" {
        let actual = identity::compute_hash();
        if actual != expected {
            eprintln!("FATAL: identity.json has been modified since build.");
            eprintln!("Expected hash: {expected}");
            eprintln!("Actual hash:   {actual}");
            eprintln!("Rebuild with: cargo build --release");
            std::process::exit(1);
        }
    }

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Serve { port, watchdog: wd }) => {
            if wd {
                let pid = std::process::id();
                let state = state_dir();
                eprintln!("Starting watchdog for PID {pid}...");
                let wd_handle = tokio::spawn(watchdog::start_watchdog(pid, port, state));
                let serve_handle = tokio::spawn(mcp_http::serve(port));
                tokio::select! {
                    _ = wd_handle => {},
                    _ = serve_handle => {},
                }
            } else {
                mcp_http::serve(port).await;
            }
        }
        Some(Commands::Watch { pid, port }) => {
            watchdog::start_watchdog(pid, port, state_dir()).await;
        }
        None => {
            // 启动前检查崩溃恢复
            let state_dir = state_dir();
            let session_store = Arc::new(JsonlSessionStore::new(&state_dir));

            if let Some(recovery) = session_store.check_recovery() {
                let choice = show_crash_recovery(&recovery);
                match choice {
                    RecoveryChoice::Resume => {
                        eprintln!("恢复上次会话...\n");
                        // TODO: 从 session 加载消息历史继续
                        run_repl_with_session(cli.max_turns, session_store, recovery).await;
                        return;
                    }
                    RecoveryChoice::Restart => {
                        eprintln!("开始新会话。\n");
                    }
                    RecoveryChoice::Diagnose => {
                        show_diagnosis(&recovery);
                        eprintln!();
                    }
                }
            }

            if let Some(prompt) = cli.prompt {
                run_single(prompt, cli.max_turns).await;
            } else {
                run_repl(cli.max_turns).await;
            }
        }
    }
}

async fn run_single(prompt: String, max_turns: u32) {
    let (provider, tools, neuro, cancel, neuro_tx) = setup().await;
    let system_prompt = wake::assemble();
    let messages = vec![Message::user(&prompt)];

    let stream = run_loop(
        provider,
        tools,
        Arc::new(katherine_engine::memory::libsql_store::LibSqlHub::new(std::path::PathBuf::from("katherine-memories/katherine.db")).expect("libSQL")),
        neuro.clone(),
        messages,
        system_prompt,
        LoopConfig {
            max_turns,
            neuro_tx,
            ..LoopConfig::default()
        },
        cancel,
    );

    tokio::pin!(stream);
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Text { text } => {
                print!("{}", text);
            }
            AgentEvent::ToolCall { name, .. } => {
                eprintln!("\n[tool:{}]", name);
            }
            AgentEvent::ToolResult { result, .. } => {
                if result.is_error {
                    eprintln!("[error: {}]", result.content);
                }
            }
            AgentEvent::Done { .. } => break,
            AgentEvent::Error { error } => {
                eprintln!("\n[Error: {}]", error);
            }
            _ => {}
        }
    }
    println!();
}

async fn run_repl(max_turns: u32) {
    let (provider, tools, neuro, cancel, neuro_tx) = setup().await;
    let system_prompt = wake::assemble();
    let db_path = std::env::var("KATHERINE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("katherine-memories")
        .join("katherine.db");
    let hub: Arc<dyn katherine_core::hub::Hub> =
        match katherine_engine::memory::libsql_store::LibSqlHub::new(db_path) {
            Ok(store) => {
                let report = store.consolidate().await;
                if report.total > 0 {
                    eprintln!(
                        "mem: {} total, {} archived",
                        report.total, report.archived
                    );
                }
                Arc::new(store)
            }
            Err(e) => {
                eprintln!("Failed to init libSQL: {e}. Falling back to HttpHub.");
                Arc::new(katherine_engine::memory::libsql_store::LibSqlHub::new(std::path::PathBuf::from("katherine-memories/katherine.db")).expect("libSQL"))
            }
        };

    eprintln!("Katherine ready. Type /exit to quit.\n");

    let stdin = std::io::stdin();
    let mut messages: Vec<Message> = Vec::new();

    for line in stdin.lock().lines() {
        let input = match line {
            Ok(l) => l.trim().to_string(),
            Err(_) => break,
        };
        if input.is_empty() {
            continue;
        }
        if input == "/exit" || input == "/quit" {
            break;
        }

        messages.push(Message::user(&input));

        let stream = run_loop(
            provider.clone(),
            tools.clone(),
            hub.clone(),
            neuro.clone(),
            messages.clone(),
            system_prompt.clone(),
            LoopConfig {
                max_turns,
                neuro_tx: neuro_tx.clone(),
                ..LoopConfig::default()
            },
            cancel.clone(),
        );

        tokio::pin!(stream);
        let mut text_out = String::new();
        while let Some(ev) = stream.next().await {
            match ev {
                AgentEvent::Text { text } => {
                    print!("{}", text);
                    text_out.push_str(&text);
                }
                AgentEvent::ToolCall { name, .. } => {
                    eprintln!("\n[tool:{}]", name);
                }
                AgentEvent::ToolResult { result, .. } => {
                    if result.is_error {
                        eprintln!("[error: {}]", result.content);
                    }
                }
                AgentEvent::Done { .. } => break,
                AgentEvent::Error { error } => {
                    eprintln!("\n[Error: {}]", error);
                }
                _ => {}
            }
        }

        if !text_out.is_empty() {
            messages.push(Message::assistant(vec![
                katherine_core::types::ContentBlock::Text { text: text_out },
            ]));
            // 实际上完整的 assistant 消息在 loop 内部已追加到 messages 流里
            // 但 REPL 中 messages 是外部管理——这里做简单文本记录
        }

        // 截断
        if messages.len() > 40 {
            let remove = messages.len() - 40;
            messages.drain(0..remove);
        }

        println!();
    }

    eprintln!("\nBye.");
}

async fn setup() -> (
    Arc<dyn katherine_core::provider::LlmProvider>,
    Arc<ToolRegistry>,
    Arc<MemNeuro>,
    CancellationToken,
    Option<tokio::sync::mpsc::UnboundedSender<katherine_engine::neuro_v3::NeuroEvent>>,
) {
    let db_path = std::env::var("KATHERINE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("katherine-memories")
        .join("katherine.db");
    let (hub, neuro_observer) =
        match katherine_engine::memory::libsql_store::LibSqlHub::new_with_conn(db_path) {
            Ok((store, conn)) => {
                let report = store.consolidate().await;
                if report.total > 0 {
                    eprintln!(
                        "mem: {} total, {} archived",
                        report.total, report.archived
                    );
                }
                let hub: Arc<dyn katherine_core::hub::Hub> = Arc::new(store);
                let neuro = Arc::new(MemNeuro::new());
                let observer = katherine_engine::neuro_v3::NeuroObserver::spawn(conn, neuro.clone());
                (hub, (neuro, Some(observer)))
            }
            Err(e) => {
                eprintln!("Failed to init libSQL: {e}.");
                let hub: Arc<dyn katherine_core::hub::Hub> =
                    Arc::new(katherine_engine::memory::libsql_store::LibSqlHub::new(
                        std::path::PathBuf::from("katherine-memories/katherine.db"),
                    ).expect("libSQL"));
                let neuro = Arc::new(MemNeuro::new());
                (hub, (neuro, None))
            }
        };
    let neuro = neuro_observer.0;
    let observer = neuro_observer.1;

    let provider: Arc<dyn katherine_core::provider::LlmProvider> = match DeepSeekProvider::from_env()
    {
        Ok(p) => {
            eprintln!("Provider: {}", p.model_id());
            Arc::new(p)
        }
        Err(e) => {
            eprintln!("Warning: Failed to create provider: {e}. Using mock.");
            eprintln!("Set ANTHROPIC_AUTH_TOKEN to use DeepSeek.");
            Arc::new(MockFallbackProvider)
        }
    };
    let tools = Arc::new(ToolRegistry::with_defaults(hub, neuro.clone()));
    let cancel = CancellationToken::new();

    let neuro_tx = observer.as_ref().map(|o| o.tx.clone());
    (provider, tools, neuro, cancel, neuro_tx)
}

// ── Fallback mock provider ─────────────────────────────

use std::pin::Pin;
use std::task::{Context, Poll};
use futures::Stream;
use katherine_core::error::EngineError;
use katherine_core::event::StreamEvent;
use katherine_core::provider::{LlmProvider, Request};

struct MockFallbackProvider;

impl LlmProvider for MockFallbackProvider {
    fn model_id(&self) -> &str {
        "mock:fallback"
    }
    fn stream(
        &self,
        _request: Request,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, EngineError>> + Send + '_>> {
        Box::pin(MockFallbackStream { sent: false, stop_sent: false })
    }
}

struct MockFallbackStream {
    sent: bool,
    stop_sent: bool,
}

impl Stream for MockFallbackStream {
    type Item = Result<StreamEvent, EngineError>;
    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if !self.sent {
            self.sent = true;
            Poll::Ready(Some(Ok(StreamEvent::TextDelta {
                text: "(No provider configured. Set ANTHROPIC_AUTH_TOKEN to use DeepSeek.)\n".into(),
            })))
        } else if !self.stop_sent {
            self.stop_sent = true;
            Poll::Ready(Some(Ok(StreamEvent::MessageStop)))
        } else {
            Poll::Ready(None)
        }
    }
}

// ── 崩溃恢复 ────────────────────────────────────────────

use katherine_engine::persistence::session::SessionRecovery;

enum RecoveryChoice {
    Resume,
    Restart,
    Diagnose,
}

fn state_dir() -> PathBuf {
    std::env::var("KATHERINE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("state")
}

fn show_crash_recovery(recovery: &SessionRecovery) -> RecoveryChoice {
    eprintln!();
    eprintln!("╔══════════════════════════════════════╗");
    eprintln!("║  ⚠ 检测到上次会话未正常结束 (崩溃) ║");
    eprintln!("╠══════════════════════════════════════╣");
    eprintln!("║ 工作目录: {:25} ║", truncate(&recovery.cwd, 25));
    eprintln!("║ 模型:     {:25} ║", truncate(&recovery.model, 25));
    eprintln!("╠══════════════════════════════════════╣");
    eprintln!("║ 崩溃前最后的事件:                   ║");
    for ev in &recovery.last_events {
        eprintln!("║   • {:31} ║", truncate(ev, 31));
    }
    eprintln!("╠══════════════════════════════════════╣");
    eprintln!("║ [R] 恢复   — 继续上次对话           ║");
    eprintln!("║ [N] 新会话 — 从干净开始             ║");
    eprintln!("║ [D] 诊断   — 让我先分析问题         ║");
    eprintln!("╚══════════════════════════════════════╝");
    eprintln!();

    loop {
        eprint!("选择 [R/N/D]: ");
        io::stderr().flush().ok();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            return RecoveryChoice::Restart;
        }
        match input.trim().to_uppercase().as_str() {
            "R" => return RecoveryChoice::Resume,
            "N" => return RecoveryChoice::Restart,
            "D" => return RecoveryChoice::Diagnose,
            _ => eprintln!("请输入 R, N 或 D"),
        }
    }
}

fn show_diagnosis(recovery: &SessionRecovery) {
    eprintln!();
    eprintln!("── 诊断报告 ──");
    eprintln!("Session: {}", recovery.session_id);
    eprintln!("消息数:  {}", recovery.entries.len());
    eprintln!();

    // 分析崩溃原因
    let mut tool_calls = 0u32;
    let mut tool_errors = 0u32;
    let mut last_error = None;

    for entry in &recovery.entries {
        match entry {
            SessionEntry::Event { payload, .. } => {
                let ev_type = payload["type"].as_str().unwrap_or("");
                match ev_type {
                    "tool_call" => tool_calls += 1,
                    "tool_result" => {
                        if payload["result"]["is_error"].as_bool().unwrap_or(false) {
                            tool_errors += 1;
                            last_error = payload["result"]["content"].as_str().map(|s| s.to_string());
                        }
                    }
                    "error" => {
                        last_error = payload["error"].as_str().map(|s| s.to_string());
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    eprintln!("工具调用: {tool_calls} 次");
    eprintln!("工具错误: {tool_errors} 次");
    if let Some(err) = last_error {
        eprintln!("最后错误: {err}");
    }
    eprintln!();
    eprintln!("可能的崩溃原因:");
    if tool_errors > 0 {
        eprintln!("  - 工具执行失败（{} 次工具错误）", tool_errors);
    }
    if recovery.entries.len() > 100 {
        eprintln!("  - 消息历史过长（{} 条）", recovery.entries.len());
    }
    eprintln!("  - 进程被外部终止（Ctrl+C / OOM / 系统关机）");
    eprintln!("  - Provider API 错误或超时");
    eprintln!();
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max - 3).collect::<String>())
    }
}

async fn run_repl_with_session(
    max_turns: u32,
    _session_store: Arc<JsonlSessionStore>,
    _recovery: SessionRecovery,
) {
    // TODO Phase 7b: 从 session 恢复消息历史
    eprintln!("(从崩溃恢复——消息历史恢复功能待实现)");
    run_repl(max_turns).await;
}
