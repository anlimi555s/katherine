// tools/mod.rs — ToolRegistry + 工具注册。
// 线性查找足够——10 个工具不需要哈希表。

use std::sync::Arc;

use katherine_core::hub::Hub;
use katherine_core::neuro::Neuro;
use katherine_core::tool::{PermissionResult, Tool, ToolDefinition};

mod base;
pub mod browser;
mod memory;
mod self_read;

pub use base::*;
pub use memory::*;
pub use self_read::*;

/// 简单注册中心。线性查找。
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry { tools: Vec::new() }
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.tools.push(Arc::new(tool));
    }

    pub fn find(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.definition().name == name)
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|t| t.definition()).collect()
    }

    /// 注册全部默认工具（含自我感知工具）。
    pub fn with_defaults(hub: Arc<dyn Hub>, neuro: Arc<dyn Neuro>) -> Self {
        let mut registry = ToolRegistry::new();
        register_base_tools(&mut registry);
        register_memory_tools(&mut registry, hub, Arc::clone(&neuro));
        register_self_tools(&mut registry, neuro);
        registry
    }

    /// 权限检查。
    /// 安全校验已迁移到 SecurityMiddleware（capabilities 声明 + deny-by-default）。
    /// 此处保持宽松——Execute 工具放行，真实安检由 SecurityMiddleware 执行。
    /// TODO: 实现用户批准流程后，Execute 工具可改为 RequiresApproval。
    pub fn check_permission(&self, name: &str) -> PermissionResult {
        match self.find(name) {
            None => PermissionResult::NotFound,
            Some(_) => PermissionResult::Allowed,
        }
    }
}

fn register_base_tools(registry: &mut ToolRegistry) {
    registry.register(base::ReadTool::new());
    registry.register(base::WriteTool::new());
    registry.register(base::EditTool::new());
    registry.register(base::BashTool::default());
    registry.register(base::GlobTool::new());
    registry.register(base::GrepTool::new());

    // BrowserTool——找不到 chrome 就跳过
    match crate::tools::browser::BrowserTool::new() {
        Ok(b) => registry.register(b),
        Err(e) => eprintln!("[browser] Not available: {e}"),
    }
}

fn register_memory_tools(registry: &mut ToolRegistry, hub: Arc<dyn Hub>, neuro: Arc<dyn Neuro>) {
    registry.register(memory::MarkMemoryTool::new(Arc::clone(&hub)));
    registry.register(memory::RecallTool::new(Arc::clone(&hub)));
    registry.register(memory::SaveDecisionTool::new(Arc::clone(&hub)));
    registry.register(memory::SelfCheckTool::new(Arc::clone(&neuro)));
}

fn register_self_tools(registry: &mut ToolRegistry, neuro: Arc<dyn Neuro>) {
    registry.register(self_read::SelfReadTool::new(neuro));
}
