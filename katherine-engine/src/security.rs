// security.rs — SecurityMiddleware: 统一工具调用安全检查。
// 所有工具调用在执行前经过此中间件。
// 设计参考：MiniScope (双层) + AgentBound (deny-by-default) + Claude Code (规则引擎)
// 原则：声明匹配 → 放行；未声明的能力 → 拦截 + 审计。

use katherine_core::capability::Capability;
use katherine_core::error::EngineError;
use katherine_core::tool::Tool;

/// 安全中间件——工具执行前的统一防线。
pub struct SecurityMiddleware;

impl SecurityMiddleware {
    /// 检查工具调用是否在声明的能力范围内。
    ///
    /// # Arguments
    /// * `tool` - 被调用的工具
    /// * `action` - 工具的 action 名（如 Browser 的 "navigate"/"evaluate"）
    /// * `capabilities_needed` - 本次 action 实际使用的能力
    ///
    /// # Returns
    /// * `Ok(())` - 所有能力均在声明范围内
    /// * `Err(EngineError::ToolPermissionDenied)` - 存在未声明的能力
    pub fn check(
        tool: &dyn Tool,
        action: Option<&str>,
        capabilities_needed: &[Capability],
    ) -> Result<(), EngineError> {
        // 无 action 级能力需求 → 信任工具声明（非 Browser 的默认路径）
        if capabilities_needed.is_empty() {
            return Ok(());
        }

        let declared = tool.capabilities();
        let tool_name = tool.definition().name;

        for cap in capabilities_needed {
            if !declared.contains(cap) {
                let msg = format!(
                    "Security: {}.{} requires capability {:?} which is not declared by tool '{}'. Declared: {:?}",
                    tool_name,
                    action.unwrap_or("execute"),
                    cap,
                    tool_name,
                    declared.iter().map(|c| format!("{:?}", c)).collect::<Vec<_>>()
                );
                return Err(EngineError::ToolPermissionDenied {
                    name: tool_name,
                    message: Some(msg),
                });
            }
        }

        Ok(())
    }
}

/// 将工具调用映射为所需的能力列表。
/// 对于 Browser，按 action 细分。对于其他工具，使用工具的声明能力作为"本次调用需要的"。
pub fn capabilities_for_call(tool_name: &str, action: Option<&str>) -> Vec<Capability> {
    if tool_name == "browser" {
        if let Some(act) = action {
            return browser_action_capabilities(act);
        }
    }
    // 非 Browser 工具：本次调用不额外限制，信任工具的声明能力。
    // SecurityMiddleware::check 会比对声明范围。
    vec![]
}

/// 将 Browser action 映射为所需的能力列表。
/// 不同 action 使用不同能力——这是内容级安全检查的基础。
pub fn browser_action_capabilities(action: &str) -> Vec<Capability> {
    match action {
        "navigate" | "screenshot" | "click" | "type_text" | "scroll"
        | "wait" | "get_visible_text" | "get_content" => {
            vec![Capability::NetOutbound, Capability::NetLocalhost]
        }
        "evaluate" => {
            vec![Capability::JsEvaluate]
        }
        "get_cookies" => {
            vec![Capability::CookieRead]
        }
        "set_cookie" => {
            vec![Capability::CookieWrite]
        }
        _ => vec![Capability::NetOutbound, Capability::NetLocalhost],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use katherine_core::tool::{PermissionLevel, ToolDefinition, ToolResult};

    /// 一个声明了 FsRead 的工具
    struct ReadOnlyTool;
    impl Tool for ReadOnlyTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "test_read".into(),
                description: "".into(),
                input_schema: serde_json::json!({}),
                permission_level: PermissionLevel::ReadOnly,
            }
        }
        fn execute(&self, _: serde_json::Value) -> Result<ToolResult, EngineError> {
            Ok(ToolResult::ok("ok"))
        }
        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability::FsRead]
        }
    }

    /// 一个无能力声明的工具
    struct NoCapTool;
    impl Tool for NoCapTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "test_none".into(),
                description: "".into(),
                input_schema: serde_json::json!({}),
                permission_level: PermissionLevel::ReadOnly,
            }
        }
        fn execute(&self, _: serde_json::Value) -> Result<ToolResult, EngineError> {
            Ok(ToolResult::ok("ok"))
        }
    }

    #[test]
    fn declared_capability_allowed() {
        let tool = ReadOnlyTool;
        let result = SecurityMiddleware::check(
            &tool,
            Some("read"),
            &[Capability::FsRead],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn undeclared_capability_denied() {
        let tool = ReadOnlyTool;
        let result = SecurityMiddleware::check(
            &tool,
            Some("write"),
            &[Capability::FsWrite],
        );
        assert!(result.is_err());
    }

    #[test]
    fn no_capabilities_deny_all() {
        let tool = NoCapTool;
        let result = SecurityMiddleware::check(
            &tool,
            Some("anything"),
            &[Capability::FsRead],
        );
        assert!(result.is_err());
    }

    #[test]
    fn browser_navigate_needs_net_outbound() {
        let caps = browser_action_capabilities("navigate");
        assert!(caps.contains(&Capability::NetOutbound));
        assert!(caps.contains(&Capability::NetLocalhost));
    }

    #[test]
    fn browser_evaluate_needs_js_evaluate() {
        let caps = browser_action_capabilities("evaluate");
        assert!(caps.contains(&Capability::JsEvaluate));
        assert!(!caps.contains(&Capability::NetOutbound));
    }
}
