// event.rs — StreamEvent (provider → loop) + AgentEvent (loop → caller).
// 对齐现有 types.ts StreamEvent 和 loop.ts yield 类型。

use serde::Serialize;

use crate::tool::ToolResult;

/// Provider 流出的原始事件。和现有 StreamEvent 对齐。
#[derive(Debug, Clone)]
pub enum StreamEvent {
    TextDelta { text: String },
    ThinkingDelta { thinking: String },
    ThinkingStart { signature: String },
    ThinkingEnd,
    ToolUseStart { id: String, name: String },
    ToolUseDelta { input_json: String },
    ToolUseEnd,
    MessageStop,
    /// SSE keep-alive 注释（DeepSeek 高负载时发送 `: keep-alive` 保持连接）。
    /// loop_.rs 应重置 TTFT 计时器，不计入响应内容。
    Heartbeat,
}

/// run_loop 产出的事件。比 StreamEvent 高一层——含工具结果和执行状态。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_call")]
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        result: ToolResult,
    },
    #[serde(rename = "permission_required")]
    PermissionRequired {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "turn_started")]
    TurnStarted { turn: u32 },
    #[serde(rename = "turn_completed")]
    TurnCompleted { turn: u32, tokens_used: u64 },
    #[serde(rename = "error")]
    Error { error: String },
    #[serde(rename = "done")]
    Done { reason: StopReason },
}

#[derive(Debug, Clone, Serialize)]
pub enum StopReason {
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "max_turns")]
    MaxTurns,
    #[serde(rename = "error")]
    Error,
}

impl AgentEvent {
    /// 文本事件快捷构造。
    pub fn text(t: impl Into<String>) -> Self {
        AgentEvent::Text { text: t.into() }
    }

    /// 结束事件快捷构造。
    pub fn done(reason: StopReason) -> Self {
        AgentEvent::Done { reason }
    }

    /// 工具调用事件快捷构造。
    pub fn tool_call(id: impl Into<String>, name: impl Into<String>, input: serde_json::Value) -> Self {
        AgentEvent::ToolCall {
            id: id.into(),
            name: name.into(),
            input,
        }
    }
}
