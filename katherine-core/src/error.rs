// error.rs — 类型化错误模型。
// 原则：所有错误有类型。不在输出中注入错误文本。

use std::fmt;
use std::sync::Arc;

/// 引擎所有可能错误的枚举。手动实现 Clone 以处理 Io/Json 包装。
#[derive(Debug)]
pub enum EngineError {
    // Provider 层
    ProviderAuth(String),
    ProviderRateLimited { retry_after_s: u64 },
    ProviderOverloaded,
    ProviderServer(u16, String),
    ProviderStreamInterrupted(String),

    // 工具层
    ToolNotFound(String),
    ToolExecutionFailed { name: String, message: String },
    ToolPermissionDenied { name: String, #[allow(dead_code)] message: Option<String> },
    ToolInputValidation {
        name: String,
        errors: Vec<String>,
    },

    // 消息循环
    EmptyResponse,
    MaxTurnsReached(u32),

    // Hub
    HubUnreachable(String),
    HubTimeout,

    // 通用（Arc 包装以支持 Clone）
    Io(Arc<std::io::Error>),
    Json(Arc<serde_json::Error>),
    Config(String),
}

impl Clone for EngineError {
    fn clone(&self) -> Self {
        match self {
            EngineError::ProviderAuth(s) => EngineError::ProviderAuth(s.clone()),
            EngineError::ProviderRateLimited { retry_after_s } => EngineError::ProviderRateLimited { retry_after_s: *retry_after_s },
            EngineError::ProviderOverloaded => EngineError::ProviderOverloaded,
            EngineError::ProviderServer(c, s) => EngineError::ProviderServer(*c, s.clone()),
            EngineError::ProviderStreamInterrupted(s) => EngineError::ProviderStreamInterrupted(s.clone()),
            EngineError::ToolNotFound(s) => EngineError::ToolNotFound(s.clone()),
            EngineError::ToolExecutionFailed { name, message } => EngineError::ToolExecutionFailed { name: name.clone(), message: message.clone() },
            EngineError::ToolPermissionDenied { name, message } => EngineError::ToolPermissionDenied { name: name.clone(), message: message.clone() },
            EngineError::ToolInputValidation { name, errors } => EngineError::ToolInputValidation { name: name.clone(), errors: errors.clone() },
            EngineError::EmptyResponse => EngineError::EmptyResponse,
            EngineError::MaxTurnsReached(n) => EngineError::MaxTurnsReached(*n),
            EngineError::HubUnreachable(s) => EngineError::HubUnreachable(s.clone()),
            EngineError::HubTimeout => EngineError::HubTimeout,
            EngineError::Io(e) => EngineError::Io(Arc::clone(e)),
            EngineError::Json(e) => EngineError::Json(Arc::clone(e)),
            EngineError::Config(s) => EngineError::Config(s.clone()),
        }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::ProviderAuth(msg) => write!(f, "Provider auth error: {msg}"),
            EngineError::ProviderRateLimited { retry_after_s } => {
                write!(f, "Rate limited, retry after {retry_after_s}s")
            }
            EngineError::ProviderOverloaded => write!(f, "Provider overloaded (529)"),
            EngineError::ProviderServer(code, msg) => {
                write!(f, "Provider server error {code}: {msg}")
            }
            EngineError::ProviderStreamInterrupted(msg) => {
                write!(f, "Stream interrupted: {msg}")
            }
            EngineError::ToolNotFound(name) => write!(f, "Tool not found: {name}"),
            EngineError::ToolExecutionFailed { name, message } => {
                write!(f, "Tool '{name}' failed: {message}")
            }
            EngineError::ToolPermissionDenied { name, message } => {
                if let Some(msg) = message {
                    write!(f, "Permission denied: {msg}")
                } else {
                    write!(f, "Permission denied: {name}")
                }
            }
            EngineError::ToolInputValidation { name, errors } => {
                write!(f, "Tool '{name}' input validation: {errors:?}")
            }
            EngineError::EmptyResponse => {
                write!(f, "Model returned empty response — no text and no tool calls")
            }
            EngineError::MaxTurnsReached(n) => write!(f, "Max turns reached: {n}"),
            EngineError::HubUnreachable(msg) => write!(f, "Hub unreachable: {msg}"),
            EngineError::HubTimeout => write!(f, "Hub timeout"),
            EngineError::Io(e) => write!(f, "I/O error: {e}"),
            EngineError::Json(e) => write!(f, "JSON error: {e}"),
            EngineError::Config(msg) => write!(f, "Config error: {msg}"),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EngineError::Io(e) => Some(&**e),
            EngineError::Json(e) => Some(&**e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        EngineError::Io(Arc::new(e))
    }
}

impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        EngineError::Json(Arc::new(e))
    }
}

impl EngineError {
    /// 判断错误是否可重试。
    /// 可重试：RateLimited / Overloaded / 5xx / StreamInterrupted
    /// 不可重试：Auth / 4xx client errors / Config / 其他
    pub fn is_transient(&self) -> bool {
        match self {
            EngineError::ProviderRateLimited { .. } => true,
            EngineError::ProviderOverloaded => true,
            EngineError::ProviderServer(code, _) => *code >= 500,
            EngineError::ProviderStreamInterrupted(_) => true,
            // 不可重试
            EngineError::ProviderAuth(_) => false,
            EngineError::ToolNotFound(_)
            | EngineError::ToolExecutionFailed { .. }
            | EngineError::ToolPermissionDenied { .. }
            | EngineError::ToolInputValidation { .. }
            | EngineError::EmptyResponse
            | EngineError::MaxTurnsReached(_)
            | EngineError::HubUnreachable(_)
            | EngineError::HubTimeout
            | EngineError::Io(_)
            | EngineError::Json(_)
            | EngineError::Config(_) => false,
        }
    }
}

/// 快捷构造：工具未找到。
pub fn tool_not_found(name: impl Into<String>) -> EngineError {
    EngineError::ToolNotFound(name.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_not_empty() {
        let errors = [
            EngineError::EmptyResponse,
            EngineError::ToolNotFound("Test".into()),
            EngineError::Config("bad".into()),
            EngineError::ProviderAuth("token".into()),
        ];
        for e in &errors {
            let s = e.to_string();
            assert!(!s.is_empty(), "Error display should not be empty: {e:?}");
        }
    }

    #[test]
    fn io_error_conversion() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "nope");
        let eng: EngineError = io.into();
        assert!(matches!(eng, EngineError::Io(_)));
    }

    #[test]
    fn json_error_conversion() {
        let json = serde_json::from_str::<serde_json::Value>("bad").unwrap_err();
        let eng: EngineError = json.into();
        assert!(matches!(eng, EngineError::Json(_)));
    }
}
