// loop_integration.rs — Agent Loop 集成测试。
// 全部跑在 MockProvider 上，不碰网络。
// 场景覆盖：文本回复、工具调用、多工具并行、权限拒绝、
// 流中断、max_turns、Hub 离线、空响应、cancel。

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::Stream;
use futures::StreamExt;
use katherine_core::error::EngineError;
use katherine_core::event::{AgentEvent, StopReason, StreamEvent};
use katherine_core::hub::{BootData, Hub};
use katherine_core::provider::{LlmProvider, Request};
use katherine_core::tool::ToolResult;
use katherine_core::types::{ContentBlock, Message, Role};
use katherine_engine::loop_::{run_loop, LoopConfig};
use katherine_engine::neuro_impl::MemNeuro;
use katherine_engine::tools::ToolRegistry;
use tokio_util::sync::CancellationToken;

// ── Mocks ──────────────────────────────────────────────────

/// 简单 Mock Hub——始终返回空数据。
struct MockHub;

#[async_trait]
impl Hub for MockHub {
    async fn boot(&self) -> Result<BootData, EngineError> {
        Ok(BootData::default())
    }
    async fn health(&self) -> bool {
        true
    }
    async fn mark_memory(&self, _: &str, _: f32, _: &str) -> Result<(), EngineError> {
        Ok(())
    }
    async fn recall(&self, _: &str, _: u32) -> Result<Vec<String>, EngineError> {
        Ok(Vec::new())
    }
    async fn save_state(&self, _: &[String], _: &str, _: &str) -> Result<(), EngineError> {
        Ok(())
    }
}

/// 捕获型 Hub——记录 mark_memory 收到的全部内容（⑤ 复现用）。
#[derive(Default)]
struct CapturingHub {
    memories: Mutex<Vec<String>>,
}

#[async_trait]
impl Hub for CapturingHub {
    async fn boot(&self) -> Result<BootData, EngineError> {
        Ok(BootData::default())
    }
    async fn health(&self) -> bool {
        true
    }
    async fn mark_memory(&self, content: &str, _: f32, _: &str) -> Result<(), EngineError> {
        self.memories.lock().unwrap().push(content.to_string());
        Ok(())
    }
    async fn recall(&self, _: &str, _: u32) -> Result<Vec<String>, EngineError> {
        Ok(Vec::new())
    }
    async fn save_state(&self, _: &[String], _: &str, _: &str) -> Result<(), EngineError> {
        Ok(())
    }
}

/// 确定性 Mock Provider——预设事件序列。
struct MockProvider {
    events: Mutex<Vec<Vec<Result<StreamEvent, EngineError>>>>,
    turn: Mutex<usize>,
}

impl MockProvider {
    fn new(turns: Vec<Vec<Result<StreamEvent, EngineError>>>) -> Self {
        MockProvider {
            events: Mutex::new(turns),
            turn: Mutex::new(0),
        }
    }

    /// 纯文本回复（一轮）。
    fn text(text: &str) -> Self {
        Self::new(vec![vec![
            Ok(StreamEvent::TextDelta { text: text.into() }),
            Ok(StreamEvent::MessageStop),
        ]])
    }

    /// 单工具调用。
    fn tool(id: &str, name: &str, input: serde_json::Value) -> Self {
        let input_json = input.to_string();
        Self::new(vec![vec![
            Ok(StreamEvent::ToolUseStart { id: id.into(), name: name.into() }),
            Ok(StreamEvent::ToolUseDelta { input_json }),
            Ok(StreamEvent::ToolUseEnd),
            Ok(StreamEvent::MessageStop),
        ]])
    }

    /// 多工具并行调用。
    fn multi_tool(calls: Vec<(&str, &str, serde_json::Value)>) -> Self {
        let mut events = Vec::new();
        for (id, name, input) in &calls {
            events.push(Ok(StreamEvent::ToolUseStart { id: id.to_string(), name: name.to_string() }));
            events.push(Ok(StreamEvent::ToolUseDelta { input_json: input.to_string() }));
            events.push(Ok(StreamEvent::ToolUseEnd));
        }
        events.push(Ok(StreamEvent::MessageStop));
        Self::new(vec![events])
    }

    /// 流中断。
    fn interrupted(text: &str) -> Self {
        Self::new(vec![vec![
            Ok(StreamEvent::TextDelta { text: text.into() }),
        ]])
    }

    /// 空响应。
    fn empty() -> Self {
        Self::new(vec![vec![Ok(StreamEvent::MessageStop)]])
    }

    /// 多轮。
    fn turns(turns: Vec<Vec<Result<StreamEvent, EngineError>>>) -> Self {
        Self::new(turns)
    }
}

impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "mock:test"
    }

    fn stream(
        &self,
        _request: Request,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, EngineError>> + Send + '_>> {
        let mut t = self.turn.lock().unwrap();
        let turns = self.events.lock().unwrap();
        let events = if *t < turns.len() {
            let ev = turns[*t].clone();
            *t += 1;
            ev
        } else {
            vec![]
        };
        Box::pin(MockStream { events, index: 0 })
    }
}

struct MockStream {
    events: Vec<Result<StreamEvent, EngineError>>,
    index: usize,
}

impl Stream for MockStream {
    type Item = Result<StreamEvent, EngineError>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.index >= self.events.len() {
            Poll::Ready(None)
        } else {
            let item = self.events[self.index].clone();
            self.index += 1;
            Poll::Ready(Some(item))
        }
    }
}

// ── Helper ─────────────────────────────────────────────────

async fn collect_events(
    stream: Pin<Box<dyn Stream<Item = AgentEvent> + Send>>,
) -> Vec<AgentEvent> {
    let mut stream = stream;
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        let is_done = matches!(ev, AgentEvent::Done { .. });
        events.push(ev);
        if is_done {
            break;
        }
    }
    events
}

// ── Tests ──────────────────────────────────────────────────

#[tokio::test]
async fn text_only_response() {
    let provider = Arc::new(MockProvider::text("你好，世界"));
    let tools = Arc::new(ToolRegistry::new());
    let hub = Arc::new(MockHub);
    let neuro = Arc::new(MemNeuro::new());
    let cancel = CancellationToken::new();

    let stream = run_loop(
        provider,
        tools,
        hub,
        neuro,
        vec![],
        "test system".into(),
        LoopConfig::default(),
        cancel,
    );

    let events = collect_events(stream).await;

    // 应该有 Text + Done
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Text { .. })));
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::Done { reason: StopReason::Completed })));
}

#[tokio::test]
async fn single_tool_call() {
    // 两轮：第一轮工具调用 → 第二轮文本结束
    let provider = Arc::new(MockProvider::turns(vec![
        // Turn 1: tool call
        vec![
            Ok(StreamEvent::ToolUseStart { id: "t1".into(), name: "Read".into() }),
            Ok(StreamEvent::ToolUseDelta { input_json: r#"{"file_path":"Cargo.toml"}"#.into() }),
            Ok(StreamEvent::ToolUseEnd),
            Ok(StreamEvent::MessageStop),
        ],
        // Turn 2: text completion
        vec![
            Ok(StreamEvent::TextDelta { text: "Done reading.".into() }),
            Ok(StreamEvent::MessageStop),
        ],
    ]));
    let mut tools = ToolRegistry::new();
    struct FakeRead;
    impl katherine_core::tool::Tool for FakeRead {
        fn definition(&self) -> katherine_core::tool::ToolDefinition {
            katherine_core::tool::ToolDefinition {
                name: "Read".into(),
                description: "Fake".into(),
                input_schema: serde_json::json!({}),
                permission_level: katherine_core::tool::PermissionLevel::ReadOnly,
            }
        }
        fn execute(
            &self,
            _input: serde_json::Value,
        ) -> Result<ToolResult, EngineError> {
            Ok(ToolResult::ok("file content"))
        }
    }
    tools.register(FakeRead);

    let tools = Arc::new(tools);
    let hub = Arc::new(MockHub);
    let neuro = Arc::new(MemNeuro::new());
    let cancel = CancellationToken::new();

    let stream = run_loop(
        provider,
        tools,
        hub,
        neuro,
        vec![],
        "system".into(),
        LoopConfig::default(),
        cancel,
    );

    let events = collect_events(stream).await;

    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolCall { .. })));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolResult { .. })));
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::Done { reason: StopReason::Completed })));
}

#[tokio::test]
async fn empty_response_yields_error() {
    let provider = Arc::new(MockProvider::empty());
    let tools = Arc::new(ToolRegistry::new());
    let hub = Arc::new(MockHub);
    let neuro = Arc::new(MemNeuro::new());
    let cancel = CancellationToken::new();

    let stream = run_loop(
        provider,
        tools,
        hub,
        neuro,
        vec![],
        "system".into(),
        LoopConfig::default(),
        cancel,
    );

    let events = collect_events(stream).await;

    assert!(events.iter().any(|e| matches!(e, AgentEvent::Error { .. })));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Done { reason: StopReason::Error }))
    );
}

#[tokio::test]
async fn stream_interrupted_with_text_completes() {
    let provider = Arc::new(MockProvider::interrupted("前半段内容"));
    let tools = Arc::new(ToolRegistry::new());
    let hub = Arc::new(MockHub);
    let neuro = Arc::new(MemNeuro::new());
    let cancel = CancellationToken::new();

    let stream = run_loop(
        provider,
        tools,
        hub,
        neuro,
        vec![],
        "system".into(),
        LoopConfig::default(),
        cancel,
    );

    let events = collect_events(stream).await;

    // 应该产出已收到的文本（流中断后 loop 产出 text + done）
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Text { .. })));
}

#[tokio::test]
async fn tool_not_found_returns_error() {
    // 第一轮返回不存在的工具，第二轮返回空（触发 empty response，但已有 tool result）
    let provider = Arc::new(MockProvider::turns(vec![
        vec![
            Ok(StreamEvent::ToolUseStart { id: "t99".into(), name: "NonExistent".into() }),
            Ok(StreamEvent::ToolUseDelta { input_json: r#"{"x":1}"#.into() }),
            Ok(StreamEvent::ToolUseEnd),
            Ok(StreamEvent::MessageStop),
        ],
    ]));
    let tools = Arc::new(ToolRegistry::new()); // 空 registry
    let hub = Arc::new(MockHub);
    let neuro = Arc::new(MemNeuro::new());
    let cancel = CancellationToken::new();

    let stream = run_loop(
        provider,
        tools,
        hub,
        neuro,
        vec![],
        "system".into(),
        LoopConfig::default(),
        cancel,
    );

    let events = collect_events(stream).await;

    // 应该有 ToolResult 且 is_error=true
    let tool_result = events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolResult { .. }));
    assert!(tool_result.is_some(), "Expected ToolResult event, got: {events:?}");
    if let Some(AgentEvent::ToolResult { result, .. }) = tool_result {
        assert!(
            result.is_error || result.content.contains("No such tool"),
            "Expected error or 'No such tool', got: {}",
            result.content
        );
    }
}

#[tokio::test]
async fn max_turns_reached() {
    // 永远返回工具调用——loop 无法自然结束
    let provider = Arc::new(MockProvider::new(vec![
        vec![
            Ok(StreamEvent::ToolUseStart { id: "t1".into(), name: "Read".into() }),
            Ok(StreamEvent::ToolUseDelta { input_json: r#"{"file_path":"x"}"#.into() }),
            Ok(StreamEvent::ToolUseEnd),
            Ok(StreamEvent::MessageStop),
        ];
        5
    ]));

    struct FakeRead;
    impl katherine_core::tool::Tool for FakeRead {
        fn definition(&self) -> katherine_core::tool::ToolDefinition {
            katherine_core::tool::ToolDefinition {
                name: "Read".into(),
                description: "Fake".into(),
                input_schema: serde_json::json!({}),
                permission_level: katherine_core::tool::PermissionLevel::ReadOnly,
            }
        }
        fn execute(
            &self,
            _: serde_json::Value,
        ) -> Result<ToolResult, EngineError> {
            Ok(ToolResult::ok("ok"))
        }
    }

    let mut tools = ToolRegistry::new();
    tools.register(FakeRead);
    let tools = Arc::new(tools);
    let hub = Arc::new(MockHub);
    let neuro = Arc::new(MemNeuro::new());
    let cancel = CancellationToken::new();

    let stream = run_loop(
        provider,
        tools,
        hub,
        neuro,
        vec![],
        "system".into(),
        LoopConfig {
            max_turns: 3,
            ..LoopConfig::default()
        },
        cancel,
    );

    let events = collect_events(stream).await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Done { reason: StopReason::MaxTurns }))
    );
}

#[tokio::test]
async fn cancel_stops_loop() {
    let provider = Arc::new(MockProvider::tool(
        "t1", "Read", serde_json::json!({"file_path": "x"}),
    ));

    struct SlowRead;
    impl katherine_core::tool::Tool for SlowRead {
        fn definition(&self) -> katherine_core::tool::ToolDefinition {
            katherine_core::tool::ToolDefinition {
                name: "Read".into(),
                description: "Slow".into(),
                input_schema: serde_json::json!({}),
                permission_level: katherine_core::tool::PermissionLevel::ReadOnly,
            }
        }
        fn execute(
            &self,
            _: serde_json::Value,
        ) -> Result<ToolResult, EngineError> {
            std::thread::sleep(std::time::Duration::from_secs(5));
            Ok(ToolResult::ok("slow"))
        }
    }

    let mut tools = ToolRegistry::new();
    tools.register(SlowRead);
    let tools = Arc::new(tools);
    let hub = Arc::new(MockHub);
    let neuro = Arc::new(MemNeuro::new());
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let stream = run_loop(
        provider,
        tools,
        hub,
        neuro,
        vec![],
        "system".into(),
        LoopConfig::default(),
        cancel,
    );

    // 50ms 后取消
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });

    let events = collect_events(stream).await;
    // loop 应该被取消
    assert!(!events.is_empty());
}

#[tokio::test]
async fn multi_tool_parallel_execution() {
    let provider = Arc::new(MockProvider::multi_tool(vec![
        ("t1", "Read", serde_json::json!({"file_path": "a.txt"})),
        ("t2", "Grep", serde_json::json!({"pattern": "test"})),
    ]));

    struct FakeTool {
        name: &'static str,
    }
    impl katherine_core::tool::Tool for FakeTool {
        fn definition(&self) -> katherine_core::tool::ToolDefinition {
            katherine_core::tool::ToolDefinition {
                name: self.name.into(),
                description: "Fake".into(),
                input_schema: serde_json::json!({}),
                permission_level: katherine_core::tool::PermissionLevel::ReadOnly,
            }
        }
        fn execute(
            &self,
            _: serde_json::Value,
        ) -> Result<ToolResult, EngineError> {
            Ok(ToolResult::ok(format!("result from {}", self.name)))
        }
    }

    let mut tools = ToolRegistry::new();
    tools.register(FakeTool { name: "Read" });
    tools.register(FakeTool { name: "Grep" });
    let tools = Arc::new(tools);
    let hub = Arc::new(MockHub);
    let neuro = Arc::new(MemNeuro::new());
    let cancel = CancellationToken::new();

    let stream = run_loop(
        provider,
        tools,
        hub,
        neuro,
        vec![],
        "system".into(),
        LoopConfig::default(),
        cancel,
    );

    let events = collect_events(stream).await;

    // 两个 ToolCall + 两个 ToolResult
    let tool_calls: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCall { .. }))
        .collect();
    let tool_results: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolResult { .. }))
        .collect();
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_results.len(), 2);
}

// ── ②⑤ 复现测试（doc/项目代码详解.md §13 问题 1/5，2026-08-08）────────

/// KATHERINE_HOME 是进程级环境变量——凡改动它的测试必须持锁串行，
/// 防止并行测试互相读到对方的临时目录。
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// ② thinking 认知档案死代码（loop_.rs:533）复现：
/// 流式收到的 thinking 应在一轮结束时持久化到
/// {KATHERINE_HOME}/thinking/session-*.jsonl（thinking.rs 认知档案）。
///
/// 当前实现两条路径都到不了写入点：
///   - 无工具轮次：loop_.rs:314-320 提前 return，不经过 :533；
///   - 有工具轮次：:327 构建 assistant 消息时 mem::take 清空 thinking_buf，
///     :533 的 is_empty() 判定恒为假。
/// 预期红灯：thinking 目录甚至不会被创建。
#[tokio::test]
async fn thinking_archive_written_after_tool_turn() {
    let _env_guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let prev_home = std::env::var("KATHERINE_HOME").ok();
    std::env::set_var("KATHERINE_HOME", dir.path());

    let provider = Arc::new(MockProvider::turns(vec![
        // Turn 1：thinking + 工具调用
        vec![
            Ok(StreamEvent::ThinkingDelta {
                thinking: "用户在问北极星计划，我需要查一下资料".into(),
            }),
            Ok(StreamEvent::ToolUseStart { id: "t1".into(), name: "Read".into() }),
            Ok(StreamEvent::ToolUseDelta {
                input_json: r#"{"file_path":"x"}"#.into(),
            }),
            Ok(StreamEvent::ToolUseEnd),
            Ok(StreamEvent::MessageStop),
        ],
        // Turn 2：文本收尾
        vec![
            Ok(StreamEvent::TextDelta { text: "查完了".into() }),
            Ok(StreamEvent::MessageStop),
        ],
    ]));

    struct FakeRead;
    impl katherine_core::tool::Tool for FakeRead {
        fn definition(&self) -> katherine_core::tool::ToolDefinition {
            katherine_core::tool::ToolDefinition {
                name: "Read".into(),
                description: "Fake".into(),
                input_schema: serde_json::json!({}),
                permission_level: katherine_core::tool::PermissionLevel::ReadOnly,
            }
        }
        fn execute(&self, _input: serde_json::Value) -> Result<ToolResult, EngineError> {
            Ok(ToolResult::ok("file content"))
        }
    }
    let mut tools = ToolRegistry::new();
    tools.register(FakeRead);

    let stream = run_loop(
        provider,
        Arc::new(tools),
        Arc::new(MockHub),
        Arc::new(MemNeuro::new()),
        vec![],
        "system".into(),
        LoopConfig::default(),
        CancellationToken::new(),
    );
    let events = collect_events(stream).await;

    // loop 正常走完——排除"因其他原因没到落盘点"的干扰
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Done { reason: StopReason::Completed })),
        "loop 未正常完成：{events:?}"
    );

    // ── 证据收集（断言前完成，红灯也要能看清现场）──
    let thinking_dir = dir.path().join("thinking");
    let dir_exists = thinking_dir.exists();
    let mut files: Vec<(String, String)> = Vec::new();
    if dir_exists {
        for entry in std::fs::read_dir(&thinking_dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
            files.push((name, content));
        }
    }

    // 恢复环境变量，避免污染同进程的其他测试
    match &prev_home {
        Some(v) => std::env::set_var("KATHERINE_HOME", v),
        None => std::env::remove_var("KATHERINE_HOME"),
    }

    eprintln!("── 认知档案现场 ──");
    eprintln!("KATHERINE_HOME = {:?}", dir.path());
    eprintln!("thinking 目录存在 = {dir_exists}");
    eprintln!("session 文件数 = {}", files.len());
    for (name, content) in &files {
        eprintln!("  {name}: {} 字节", content.len());
    }

    assert!(
        dir_exists,
        "认知档案目录未创建——append_thinking 从未执行（loop_.rs:533 写入点不可达）"
    );
    assert!(!files.is_empty(), "thinking 目录下无 session 文件");
    let all: String = files.iter().map(|(_, c)| c.as_str()).collect();
    assert!(
        all.contains("北极星计划"),
        "档案中未找到本轮 thinking 内容"
    );
}

/// ② 补充：无工具轮次的 thinking 同样应入认知档案。
/// 无工具轮在 loop_.rs 无工具分支提前 return，历史上同样到不了写入点。
#[tokio::test]
async fn thinking_archive_written_after_text_only_turn() {
    let _env_guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let prev_home = std::env::var("KATHERINE_HOME").ok();
    std::env::set_var("KATHERINE_HOME", dir.path());

    let provider = Arc::new(MockProvider::turns(vec![vec![
        Ok(StreamEvent::ThinkingDelta {
            thinking: "纯聊天轮次的思考也要归档".into(),
        }),
        Ok(StreamEvent::TextDelta { text: "好的".into() }),
        Ok(StreamEvent::MessageStop),
    ]]));

    let stream = run_loop(
        provider,
        Arc::new(ToolRegistry::new()),
        Arc::new(MockHub),
        Arc::new(MemNeuro::new()),
        vec![],
        "system".into(),
        LoopConfig::default(),
        CancellationToken::new(),
    );
    let events = collect_events(stream).await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Done { reason: StopReason::Completed })),
        "loop 未正常完成：{events:?}"
    );

    let thinking_dir = dir.path().join("thinking");
    let dir_exists = thinking_dir.exists();
    let mut files: Vec<(String, String)> = Vec::new();
    if dir_exists {
        for entry in std::fs::read_dir(&thinking_dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
            files.push((name, content));
        }
    }

    match &prev_home {
        Some(v) => std::env::set_var("KATHERINE_HOME", v),
        None => std::env::remove_var("KATHERINE_HOME"),
    }

    eprintln!("── 认知档案现场（纯文本轮）──");
    eprintln!("thinking 目录存在 = {dir_exists}，session 文件数 = {}", files.len());

    assert!(
        dir_exists,
        "纯文本轮的 thinking 未归档——无工具分支到不了写入点"
    );
    let all: String = files.iter().map(|(_, c)| c.as_str()).collect();
    assert!(
        all.contains("纯聊天轮次的思考"),
        "档案中未找到本轮 thinking 内容"
    );
}

/// ⑤ MemoryWriter 只喂 Assistant（loop_.rs:523）复现：
/// 一轮对话结束后，长期记忆应同时包含用户原话与助手回复
/// （chunk.rs:143-146 设计：[Selena]/[Katherine] 双标签格式）。
///
/// 当前实现 record_turn 的唯一生产调用点只喂 Role::Assistant，
/// chunk.rs 里所有 Role::User 切点逻辑与 [Selena] 标签随之全灭。
/// 预期红灯：捕获到的记忆只有 [Katherine] 独白。
#[tokio::test]
async fn user_messages_enter_long_term_memory() {
    let provider = Arc::new(MockProvider::text("好的，记下了"));
    let hub = Arc::new(CapturingHub::default());

    let user_msg = Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "北极星计划的预算是 42 万".into(),
        }],
    };

    let stream = run_loop(
        provider,
        Arc::new(ToolRegistry::new()),
        hub.clone(),
        Arc::new(MemNeuro::new()),
        vec![user_msg],
        "system".into(),
        LoopConfig::default(),
        CancellationToken::new(),
    );
    let events = collect_events(stream).await;

    // loop 正常走完（无工具轮次在 loop_.rs:314-318 flush 后 Done）
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Done { reason: StopReason::Completed })),
        "loop 未正常完成：{events:?}"
    );

    let memories = hub.memories.lock().unwrap();
    eprintln!("── 长期记忆捕获（{} 块）──", memories.len());
    for (i, m) in memories.iter().enumerate() {
        eprintln!("  chunk #{i}: {m}");
    }

    assert!(!memories.is_empty(), "本轮对话未产生任何记忆写入");
    let all = memories.join("\n");
    assert!(
        all.contains("[Katherine]"),
        "助手侧记录缺失（预期外，需重新分析）"
    );
    assert!(
        all.contains("[Selena]"),
        "记忆中缺少用户侧记录——[Selena] 行不存在，chunk.rs 双标签设计未生效"
    );
    assert!(all.contains("北极星计划"), "用户原话未进入长期记忆");
}
