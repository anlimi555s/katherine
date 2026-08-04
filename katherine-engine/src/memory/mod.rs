// memory/mod.rs — 引擎内嵌记忆系统。
// v3: libSQL 单文件 + BM25 检索 + 级联升级。

pub mod chunk;
pub mod decay;
pub mod libsql_store;
pub mod retrieval;
pub mod schema;

use std::sync::Arc;
use katherine_core::hub::Hub;
use tokio::task::JoinHandle;

/// 记忆写入器——引擎 loop 调用，实时切块存储。
/// 收集 flush 的 JoinHandle，退出时等待全部完成——防丢数据。
pub struct MemoryWriter {
    hub: Arc<dyn Hub>,
    buffer: Vec<chunk::TurnRecord>,
    mode: chunk::SessionMode,
    pending: Vec<JoinHandle<()>>,
}

impl MemoryWriter {
    pub fn new(hub: Arc<dyn Hub>) -> Self {
        MemoryWriter {
            hub,
            buffer: Vec::new(),
            mode: chunk::SessionMode::Chat,
            pending: Vec::new(),
        }
    }

    /// 记录一轮 turn。引擎在每轮结束后调用。
    pub fn record_turn(&mut self, record: chunk::TurnRecord) {
        self.buffer.push(record);

        // 每 3 轮检测一次模式（不去频繁判断）
        if self.buffer.len() % 3 == 0 {
            self.mode = chunk::detect_mode(&self.buffer);
        }

        // 检查是否产生完整块
        if let Some(chunk_text) = chunk::try_extract_chunk(&mut self.buffer, self.mode) {
            self.flush_chunk(&chunk_text);
        }
    }

    /// 强制输出当前缓冲区并等待所有 pending 写入完成。
    /// 会话退出前必须调用——不调用丢记忆。
    pub async fn flush(&mut self) {
        if !self.buffer.is_empty() {
            let text = chunk::buffer_to_text(&self.buffer);
            self.flush_chunk(&text);
            self.buffer.clear();
        }
        // 等待之前所有的 tokio::spawn 完成
        for handle in self.pending.drain(..) {
            let _ = handle.await;
        }
    }

    fn flush_chunk(&mut self, text: &str) {
        let hub = self.hub.clone();
        let content = text.to_string();
        let has_tools = content.contains("[tool:");
        let importance = chunk::score_importance(&content, has_tools, false);
        let handle = tokio::spawn(async move {
            let _ = hub.mark_memory(&content, importance, "engine").await;
        });
        self.pending.push(handle);
    }
}
