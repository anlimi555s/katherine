// hub.rs — Hub trait：记忆 + 状态持久化接口。
// 当前实现：HttpHub（:8765 HTTP）。以后：EmbeddedChroma，GrpcHub。

use async_trait::async_trait;

use crate::error::EngineError;

/// Hub 启动返回数据。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BootData {
    pub state: Option<HubState>,
    pub memories: Vec<String>,
}

/// 从 Hub 加载的跨会话状态。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HubState {
    pub mood: String,
    pub overload_risk: String,
    pub open_threads: Vec<String>,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub last_sleep: String,
}

/// Hub 接口。所有方法返回 Result——loop 不因 Hub 错误而失败。
/// 使用 #[async_trait] 以支持 dyn dispatch（Arc<dyn Hub>）。
#[async_trait]
pub trait Hub: Send + Sync {
    /// 启动：加载状态 + 最近的记忆。
    async fn boot(&self) -> Result<BootData, EngineError>;

    /// 健康检查。
    async fn health(&self) -> bool;

    /// 存入一条记忆。
    async fn mark_memory(
        &self,
        content: &str,
        importance: f32,
        source: &str,
    ) -> Result<(), EngineError>;

    /// 语义搜索记忆。
    async fn recall(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<String>, EngineError>;

    /// 保存引擎状态（休眠时调用）。
    async fn save_state(
        &self,
        threads: &[String],
        mood: &str,
        risk: &str,
    ) -> Result<(), EngineError>;
}
