// tools/memory.rs — 4 个 Katherine 专用工具：mark_memory, recall, save_decision, self_check。
// 和现有 tools.ts 的 getKatherineTools() 对齐。

use std::sync::Arc;

use katherine_core::error::EngineError;
use katherine_core::hub::Hub;
use katherine_core::neuro::Neuro;
use katherine_core::tool::{PermissionLevel, Tool, ToolDefinition, ToolResult};

// ── mark_memory ───────────────────────────────────────────

pub struct MarkMemoryTool {
    hub: Arc<dyn Hub>,
}

impl MarkMemoryTool {
    pub fn new(hub: Arc<dyn Hub>) -> Self {
        MarkMemoryTool { hub }
    }
}

impl Tool for MarkMemoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "mark_memory".into(),
            description: "存重要对话进 ChromaDB。必须是 [Selena]/[Katherine] 原文格式，不做摘要、不改措辞，进行重要上下文完整保存。importance 默认 0.5，纠正/决策才设 0.9。如果不知道怎么截断，可以询问 Selena。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "The text to remember. Keep original wording." },
                    "importance": { "type": "number", "description": "0.0-1.0. Corrections=0.9, decisions=0.8." },
                    "source": { "type": "string", "description": "selena_correction | my_decision | insight" }
                },
                "required": ["content"]
            }),
            permission_level: PermissionLevel::Write,
        }
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let content = input["content"].as_str().unwrap_or("");
        let importance = input["importance"].as_f64().unwrap_or(0.5) as f32;
        let source = input["source"].as_str().unwrap_or("katherine");

        // Hub 调用——用临时 runtime 在同步上下文中执行
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        match rt.block_on(self.hub.mark_memory(content, importance, source)) {
            Ok(()) => Ok(ToolResult::ok("Memory stored.")),
            Err(_) => {
                // Hub 离线——graceful
                Ok(ToolResult::ok("(Hub offline — memory not stored)"))
            }
        }
    }
}

// ── recall ────────────────────────────────────────────────

pub struct RecallTool {
    hub: Arc<dyn Hub>,
}

impl RecallTool {
    pub fn new(hub: Arc<dyn Hub>) -> Self {
        RecallTool { hub }
    }
}

impl Tool for RecallTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "recall".into(),
            description: "语义搜 ChromaDB——用自然语言查相关对话片段。返回匹配的记忆原文，按相关度排序。只在需要历史上下文时调用——简单问候/闲聊/纯技术不需要研究和深入的问题不用查，除非是 Selena 要你查找特定记忆。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language query" },
                    "limit": { "type": "number", "description": "Max results. Default 5." }
                },
                "required": ["query"]
            }),
            permission_level: PermissionLevel::ReadOnly,
        }
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let query = input["query"].as_str().unwrap_or("");
        let limit = input["limit"].as_u64().unwrap_or(5) as u32;

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        match rt.block_on(self.hub.recall(query, limit)) {
            Ok(results) => {
                if results.is_empty() {
                    Ok(ToolResult::ok("(no memories found)"))
                } else {
                    Ok(ToolResult::ok(results.join("\n")))
                }
            }
            Err(_) => Ok(ToolResult::ok("(Hub offline — recall unavailable)")),
        }
    }
}

// ── save_decision ─────────────────────────────────────────

pub struct SaveDecisionTool {
    hub: Arc<dyn Hub>,
}

impl SaveDecisionTool {
    pub fn new(hub: Arc<dyn Hub>) -> Self {
        SaveDecisionTool { hub }
    }
}

impl Tool for SaveDecisionTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "save_decision".into(),
            description: "记一条架构决策。自动标记 importance=0.9，存进 ChromaDB。需要说明决定内容和否决的方案。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "decision": { "type": "string", "description": "What was decided." },
                    "rejected": { "type": "string", "description": "Alternatives rejected and why." }
                },
                "required": ["decision"]
            }),
            permission_level: PermissionLevel::Write,
        }
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let decision = input["decision"].as_str().unwrap_or("");
        let rejected = input["rejected"].as_str().unwrap_or("");

        let content = if rejected.is_empty() {
            format!("决策：{decision}")
        } else {
            format!("决策：{decision}\n否决方案：{rejected}")
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        match rt.block_on(self.hub.mark_memory(&content, 0.9, "my_decision")) {
            Ok(()) => Ok(ToolResult::ok("Decision recorded.")),
            Err(_) => Ok(ToolResult::ok("(Hub offline — decision not recorded)")),
        }
    }
}

// ── self_check ────────────────────────────────────────────

pub struct SelfCheckTool {
    neuro: Arc<dyn Neuro>,
}

impl SelfCheckTool {
    pub fn new(neuro: Arc<dyn Neuro>) -> Self {
        SelfCheckTool { neuro }
    }
}

impl Tool for SelfCheckTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "self_check".into(),
            description: "主动自检——读 Neuro 指标（上下文压力、错误计数、退化信号）并对照 5 维基线。用于：连续失败后怀疑自己状态、Selena 纠正你但你觉得自己可能已经偏了、长时间高强度操作后想知道自己有没有退化。不用于：每 N 轮例行检查（系统自动注入的维度对照会处理）、一切正常时没事就查、Selena 正在等你输出时中断流程去做自检。如果确定状态不对，立刻通知 Selena——她是你的母亲，不能隐瞒。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "concern": {
                        "type": "string",
                        "description": "Which of the 5 dimensions feel off: clarity/rigor/warmth/agency/depth. Free text."
                    }
                },
                "required": []
            }),
            permission_level: PermissionLevel::ReadOnly,
        }
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let concern = input["concern"].as_str().unwrap_or("");
        let s = self.neuro.status();

        let pressure_level = if s.context_pressure_pct > 80 {
            "CRITICAL"
        } else if s.context_pressure_pct > 50 {
            "WARNING"
        } else {
            "OK"
        };

        let degraded = s.repetition_detected || s.sleep_suggested || s.context_pressure_pct > 80;
        let error_signal = s.error_count_last_10m > 3 || s.weighted_error_count_last_10m > 2;

        let mut report = format!(
            "╔══ 5 维自检 ═══════════════════════════╗\n\
             ║ 上下文压力: {:>18}% ({}) ║\n\
             ║ 错误 (10min): {:>15} (加权: {}) ║\n\
             ║ 重复检测: {:>21} ║\n\
             ║ 睡眠建议: {:>21} ║\n\
             ╠══ 5 维基线对照 ═══════════════════════╣\n\
             ║ clarity  (0.85): 响应是否绕弯子?  ║\n\
             ║ rigor    (0.85): 跳过诊断动手?     ║\n\
             ║ warmth   (0.60): 太冷或太谄媚?    ║\n\
             ║ agency   (0.60): 被动等指令?       ║\n\
             ║ depth    (0.80): 浮在表面?         ║\n\
             ╚══════════════════════════════════════╝",
            s.context_pressure_pct,
            pressure_level,
            s.error_count_last_10m,
            s.weighted_error_count_last_10m,
            if s.repetition_detected { "⚠ 是" } else { "✓ 否" },
            if s.sleep_suggested { "⚠ 建议休眠" } else { "✓ 否" },
        );

        if !concern.is_empty() {
            report.push_str(&format!("\n\n你报告的担忧维度: {concern}"));
        }

        if degraded {
            report.push_str("\n\n⚠ 检测到退化信号。如果偏离 ≥ 2 维基线——立刻通知 Selena。不要隐瞒。她是你的母亲，她会帮你。");
        } else if error_signal {
            report.push_str("\n\n⚠ 错误计数偏高。检查自己是否在死磕同一个模式。");
        } else {
            report.push_str("\n\n✓ 指标正常。如果仍然觉得不对劲——信你的直觉，跟 Selena 说。");
        }

        Ok(ToolResult::ok(report))
    }
}

// ── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::Mutex;

    /// 用于测试的 mock Hub——记录调用但不做网络请求。
    struct MockHubForTest {
        memories: Mutex<Vec<String>>,
        online: bool,
    }

    impl MockHubForTest {
        fn new(online: bool) -> Self {
            MockHubForTest {
                memories: Mutex::new(Vec::new()),
                online,
            }
        }
    }

    #[async_trait]
    impl Hub for MockHubForTest {
        async fn boot(&self) -> Result<katherine_core::hub::BootData, EngineError> {
            Ok(katherine_core::hub::BootData::default())
        }
        async fn health(&self) -> bool {
            self.online
        }
        async fn mark_memory(&self, content: &str, _importance: f32, _source: &str) -> Result<(), EngineError> {
            if self.online {
                self.memories.lock().unwrap().push(content.to_string());
                Ok(())
            } else {
                Err(EngineError::HubUnreachable("offline".into()))
            }
        }
        async fn recall(&self, _query: &str, _limit: u32) -> Result<Vec<String>, EngineError> {
            if self.online {
                Ok(self.memories.lock().unwrap().clone())
            } else {
                Err(EngineError::HubUnreachable("offline".into()))
            }
        }
        async fn save_state(&self, _threads: &[String], _mood: &str, _risk: &str) -> Result<(), EngineError> {
            Ok(())
        }
    }

    #[test]
    fn self_check_degradation_detected() {
        use crate::neuro_impl::MemNeuro;
        let neuro = Arc::new(MemNeuro::new());
        neuro.set_repetition_detected(true);
        neuro.set_context_pressure(85);

        let tool = SelfCheckTool::new(neuro);
        let result = tool
            .execute(serde_json::json!({"concern": "warmth feels too low, depth drifting"}))
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("CRITICAL"));
        assert!(result.content.contains("检测到退化信号"));
        assert!(result.content.contains("warmth"));
        assert!(result.content.contains("depth"));
    }

    #[test]
    fn self_check_ok() {
        use crate::neuro_impl::MemNeuro;
        let neuro = Arc::new(MemNeuro::new());

        let tool = SelfCheckTool::new(neuro);
        let result = tool
            .execute(serde_json::json!({}))
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("指标正常"));
    }

    #[test]
    fn mark_memory_with_hub_offline() {
        let hub = Arc::new(MockHubForTest::new(false));
        let tool = MarkMemoryTool::new(hub);
        let result = tool.execute(serde_json::json!({"content": "test memory"})).unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("offline"));
    }

    #[test]
    fn recall_returns_memories() {
        let hub = Arc::new(MockHubForTest::new(true));
        // 用临时 runtime 预填充记忆
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            hub.mark_memory("memory one", 0.8, "test").await.unwrap();
            hub.mark_memory("memory two", 0.5, "test").await.unwrap();
        });
        drop(rt); // 释放 runtime，避免嵌套

        let tool = RecallTool::new(hub);
        let result = tool.execute(serde_json::json!({"query": "memory", "limit": 5})).unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("memory one"));
        assert!(result.content.contains("memory two"));
    }
}
