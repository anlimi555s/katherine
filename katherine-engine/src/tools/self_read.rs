// tools/self_read.rs — 自我感知工具。
// Katherine 随时检查自己的 Neuro 指标——注意力熵、压力、退化信号。
// 论文驱动：LPSR(检测→回滚)+ ReliabilityBench(加权错误)+ ContinuityBench(CPR)

use std::sync::Arc;

use katherine_core::error::EngineError;
use katherine_core::neuro::Neuro;
use katherine_core::tool::{PermissionLevel, Tool, ToolDefinition, ToolResult};

pub struct SelfReadTool {
    neuro: Arc<dyn Neuro>,
}

impl SelfReadTool {
    pub fn new(neuro: Arc<dyn Neuro>) -> Self {
        SelfReadTool { neuro }
    }
}

impl Tool for SelfReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "self_read".into(),
            description: "读自己的 Neuro 指标——上下文压力、退化信号、错误计数、记忆状态。Katherine 的'信息面板'。主动自检时用，不等 Selena 问。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            permission_level: PermissionLevel::ReadOnly,
        }
    }

    fn execute(&self, _input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let s = self.neuro.status();

        let pressure_level = if s.context_pressure_pct > 80 {
            "CRITICAL"
        } else if s.context_pressure_pct > 50 {
            "WARNING"
        } else {
            "OK"
        };

        let report = format!(
            "╔══ Katherine 自检 ═══════════════════════╗\n\
             ║ 运行时间: {:>33} ║\n\
             ║ 轮次: {:>36} ║\n\
             ║ Token 用量: {:>31} ║\n\
             ║ 工具调用: {:>33} ({}) ║\n\
             ╠══ 上下文 ═══════════════════════════════╣\n\
             ║ 压力: {:>18}% ({}) ║\n\
             ║ API 调用: {:>33} ║\n\
             ║ API 错误: {:>33} ║\n\
             ╠══ 退化信号 ═════════════════════════════╣\n\
             ║ 重复检测: {:>32} ║\n\
             ║ 睡眠建议: {:>32} ║\n\
             ║ 错误 (10min): {:>28} ║\n\
             ║ 加权错误: {:>32} ║\n\
             ║ Hub 连接: {:>32} ║\n\
             ║ Provider: {:>32} ║\n\
             ╚══════════════════════════════════════════╝",
            format_duration(s.session_uptime_s),
            s.turns,
            s.tokens_used,
            s.tool_calls,
            s.tool_failures,
            s.context_pressure_pct,
            pressure_level,
            s.api_calls,
            s.api_errors,
            if s.repetition_detected { "⚠ 是" } else { "✓ 否" },
            if s.sleep_suggested { "⚠ 建议休眠" } else { "✓ 否" },
            s.error_count_last_10m,
            s.weighted_error_count_last_10m,
            if s.hub_connected { "✓ 已连接" } else { "✗ 断开" },
            s.provider_name,
        );

        Ok(ToolResult::ok(report))
    }
}

fn format_duration(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{}h {}m {}s", h, m, s)
    } else if m > 0 {
        format!("{}m {}s", m, s)
    } else {
        format!("{}s", s)
    }
}
