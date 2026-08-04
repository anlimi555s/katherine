// tool.rs — Tool trait + 定义类型。
// Tool 是唯一需要"实现"的类型——其他都是 trait + 数据。

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::EngineError;

// ── Permission ────────────────────────────────────────────
// 抄 claurst：每个工具声明权限等级，runner 统一拦截。

/// 权限等级，按危险性递增。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PermissionLevel {
    /// 只读：Glob, Grep, Read
    ReadOnly = 0,
    /// 写入：Write, Edit
    Write = 1,
    /// 执行：Bash
    Execute = 2,
    /// 危险操作（预留）
    Dangerous = 3,
    /// 永不允许
    Forbidden = 4,
}

// ── ToolDefinition ────────────────────────────────────────

/// 工具元数据——不含行为，可序列化传给模型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub permission_level: PermissionLevel,
}

impl ToolDefinition {
    /// 转为 Anthropic API 兼容的 JSON（snake_case）。
    pub fn to_api_format(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "input_schema": self.input_schema,
        })
    }
}

// ── ToolResult ────────────────────────────────────────────

/// 工具执行结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

impl fmt::Display for ToolResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.content)
    }
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        ToolResult {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        ToolResult {
            content: content.into(),
            is_error: true,
        }
    }

    /// 截断过长结果。抄 heartbit——保留 UTF-8 边界。
    pub fn truncated(mut self, max_bytes: usize) -> Self {
        if self.content.len() > max_bytes {
            // 回退到最近合法 UTF-8 边界
            let mut end = max_bytes - 40; // 留空间给截断提示
            while end > 0 && !self.content.is_char_boundary(end) {
                end -= 1;
            }
            let omitted = self.content.len() - end;
            self.content.truncate(end);
            self.content.push_str(&format!("\n[truncated: {omitted} bytes omitted]"));
        }
        self
    }
}

// ── Tool trait ────────────────────────────────────────────

/// 工具接口。每个工具是一个 struct 实现此 trait。
/// 抄 claurst 的 `self_gates()` 模式：返回 true 表示工具自己做了权限检查。
pub trait Tool: Send + Sync {
    /// 返回工具的元数据定义。
    fn definition(&self) -> ToolDefinition;

    /// 执行工具。输入是 JSON Value，输出是 ToolResult。
    fn execute(
        &self,
        input: serde_json::Value,
    ) -> Result<ToolResult, EngineError>;

    /// 工具是否自己做了安全检查？
    /// 返回 true 则 runner 跳过统一权限拦截。
    fn self_gates(&self) -> bool {
        false
    }
}

// ── PermissionResult ──────────────────────────────────────

/// 权限检查结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionResult {
    Allowed,
    RequiresApproval(PermissionLevel),
    NotFound,
}

impl PermissionResult {
    pub fn is_allowed(&self) -> bool {
        matches!(self, PermissionResult::Allowed)
    }
}
