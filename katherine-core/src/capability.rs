// capability.rs — 工具能力声明。
// 每个工具声明自己能做什么。SecurityMiddleware 在执行前校验。
// 设计参考：MiniScope (OAuth scope 层级) + AgentBound (AgentManifest) + Claude Code (permission rules)

use serde::{Deserialize, Serialize};

/// 工具能力——比 PermissionLevel 更细粒度。
/// 工具声明能力后，运行时任何超出声明的能力使用将被拦截。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    // ── 文件系统 ──
    /// 读文件系统 — Read, Grep, Glob
    FsRead,
    /// 写文件系统 — Write, Edit
    FsWrite,

    // ── 进程 ──
    /// 执行外部命令 — Bash
    ProcessSpawn,

    // ── 网络 ──
    /// 对外网络连接 — Bash(curl), Browser(navigate)
    NetOutbound,
    /// 本地回环连接 — Bash(curl localhost), Browser(localhost:9876)
    NetLocalhost,

    // ── 浏览器 ──
    /// 在页面上下文执行 JavaScript — Browser(evaluate)
    JsEvaluate,
    /// 读取 Cookie — Browser(get_cookies)
    CookieRead,
    /// 写入 Cookie — Browser(set_cookie)
    CookieWrite,

    // ── 记忆 ──
    /// 读取长期记忆 — recall
    MemoryRead,
    /// 写入长期记忆 — mark_memory, save_decision
    MemoryWrite,

    // ── 自省 ──
    /// 读取自身 Neuro 指标 — self_read, self_check
    NeuroRead,
}

impl Capability {
    /// 所有能力的人类可读标签。
    pub fn label(&self) -> &'static str {
        match self {
            Capability::FsRead => "读文件",
            Capability::FsWrite => "写文件",
            Capability::ProcessSpawn => "执行命令",
            Capability::NetOutbound => "网络连接",
            Capability::NetLocalhost => "本地网络",
            Capability::JsEvaluate => "执行 JS",
            Capability::CookieRead => "读 Cookie",
            Capability::CookieWrite => "写 Cookie",
            Capability::MemoryRead => "读记忆",
            Capability::MemoryWrite => "写记忆",
            Capability::NeuroRead => "读自省指标",
        }
    }
}
