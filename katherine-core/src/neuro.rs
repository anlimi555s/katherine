// neuro.rs — Neuro trait：观测层接口。
// 当前实现：MemNeuro（内存）。以后：HttpNeuro，WsPushNeuro。

use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::error::EngineError;

/// 工具调用统计。
#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolStat {
    pub calls: u32,
    pub failures: u32,
    pub total_duration_ms: u64,
}

/// 错误记录。
#[derive(Debug, Clone, Serialize)]
pub struct ErrorEntry {
    pub time: SystemTime,
    pub error_type: String,
    pub message: String,
    pub turn: u32,
}

/// 引擎状态快照——暴露给外部查询。
#[derive(Debug, Clone, Serialize)]
pub struct StatusSnapshot {
    pub session_uptime_s: u64,
    pub turns: u32,
    pub tokens_used: u64,
    pub tool_calls: u32,
    pub tool_failures: u32,
    pub error_count_last_10m: u32,
    pub weighted_error_count_last_10m: u32,
    pub hub_connected: bool,
    // Provider 层
    pub provider_name: String,
    pub api_calls: u32,
    pub api_errors: u32,
    pub last_api_call_ms: u64,
    pub stream_events_total: u64,
    // 上下文压力
    pub context_pressure_pct: u32,
    pub repetition_detected: bool,
    pub sleep_suggested: bool,
}

/// 观测层接口。loop 调用内部方法，外部（我/扩展/HTTP）调用查询方法。
pub trait Neuro: Send + Sync {
    // ── 内部方法（loop 调用）──

    fn record_turn(&self, turn: u32, tokens: u64, tool_calls: u32);
    fn record_tool_result(&self, name: &str, success: bool, duration_ms: u64);
    fn record_error(&self, error: &EngineError, turn: u32);
    fn heartbeat(&self);
    /// 记录一次 API 调用。
    fn record_api_call(&self, provider: &str, duration_ms: u64, success: bool, event_count: u32);

    /// 记录模型响应文本——用于重复检测（末尾段对比）。
    fn record_response(&self, turn: u32, text: &str);

    // ── 查询方法 ──

    fn status(&self) -> StatusSnapshot;
    fn tool_stats(&self, name: &str) -> Option<ToolStat>;
    fn recent_errors(&self, n: usize) -> Vec<ErrorEntry>;
    fn uptime(&self) -> Duration;

    /// 检测最近响应是否重复——连续3轮末尾相似 → 在循环。
    fn check_repetition(&self) -> bool;
    /// 最近 N 轮响应的末尾摘要（供外部诊断）。
    fn response_tail_sample(&self, n: usize) -> Vec<String>;

    /// 设置上下文压力百分比（0-100）。
    fn set_context_pressure(&self, _pct: u32) {}
    /// 标记重复检测结果。
    fn set_repetition_detected(&self, _detected: bool) {}
    /// 标记睡眠建议。
    fn set_sleep_suggested(&self, _suggested: bool) {}
    /// 加权错误计数——不同错误类型不同严重性。
    fn set_weighted_errors(&self, _count: u32) {}
    /// 标记 Hub 连通状态。
    fn set_hub_connected(&self, _connected: bool) {}
}
