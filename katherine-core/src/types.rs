// types.rs — Katherine core message types.
// 对齐现有 types.ts，但类型更严格。

use serde::{Deserialize, Serialize};

/// 对话角色。只有 user 和 assistant——没有 system（system 走系统提示）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
    Assistant,
}

/// 消息内容块。对齐 ContentBlock in types.ts。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

/// 一条消息。content 统一为 Vec<ContentBlock>——不再可以是裸 string。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: &str) -> Self {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Message {
            role: Role::Assistant,
            content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_user_serde_roundtrip() {
        let msg = Message::user("你好");
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.role, Role::User);
        assert_eq!(back.content.len(), 1);
    }

    #[test]
    fn content_block_tool_use_serde() {
        let block = ContentBlock::ToolUse {
            id: "tool_001".into(),
            name: "Read".into(),
            input: serde_json::json!({"file_path": "/tmp/x"}),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("tool_use"));
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        match back {
            ContentBlock::ToolUse { id, name, .. } => {
                assert_eq!(id, "tool_001");
                assert_eq!(name, "Read");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn content_block_tool_result_with_error() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "Error: not found".into(),
            is_error: true,
        };
        let json = serde_json::to_string(&block).unwrap();
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        match back {
            ContentBlock::ToolResult { is_error, .. } => {
                assert!(is_error);
            }
            _ => panic!("expected ToolResult"),
        }
    }
}
