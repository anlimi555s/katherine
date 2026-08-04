// provider.rs — LlmProvider trait + Request 类型。
// heartbit 思路：模型名是 provider 的属性，不在请求里。

use std::pin::Pin;

use futures::Stream;

use crate::error::EngineError;
use crate::event::StreamEvent;
use crate::tool::ToolDefinition;
use crate::types::Message;

/// 发给模型的请求。
#[derive(Debug, Clone)]
pub struct Request {
    pub messages: Vec<Message>,
    pub system_prompt: String,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: u32,
    pub stream: bool,
}

impl Request {
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Request {
            messages: Vec::new(),
            system_prompt: system_prompt.into(),
            tools: Vec::new(),
            max_tokens: 32000,
            stream: true,
        }
    }

    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    pub fn with_stream(mut self, s: bool) -> Self {
        self.stream = s;
        self
    }
}

/// LLM Provider 接口。一个方法 `stream()` ——够用。
///
/// 实现者负责：HTTP 请求、SSE 解析、重试逻辑。
/// 调用者只看到 `StreamEvent` 流。
pub trait LlmProvider: Send + Sync {
    /// 返回 "provider:model" —— 用于日志/Neuro。
    fn model_id(&self) -> &str;

    /// 流式调用模型。
    fn stream(
        &self,
        request: Request,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, EngineError>> + Send + '_>>;
}
