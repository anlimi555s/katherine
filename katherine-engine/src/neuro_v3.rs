// neuro_v3.rs — Neuro 独立观察者（v3）。
// 基于 VIGIL (2512.07094) + AgentTrace (2602.10133) + Cognitive Companion (2604.13759)。
// 与桌面设计稿 §五 保持一致。

use std::sync::Arc;
use std::time::Instant;

use katherine_core::neuro::Neuro;
use rusqlite::Connection;
use tokio::sync::mpsc;
use tokio::sync::Mutex as TokioMutex;

use crate::memory::schema;

// ── 结构化事件（loop_.rs → Neuro 观察者）──────────────────

/// 主循环发出的观测事件。
#[derive(Debug, Clone)]
pub enum NeuroEvent {
    /// 一轮 turn 完成。
    TurnCompleted {
        turn: u32,
        tokens: u64,
        tool_calls: u32,
    },
    /// 工具执行结果。
    ToolExecuted {
        name: String,
        success: bool,
        duration_ms: u64,
    },
    /// API 调用完成。
    ApiCalled {
        provider: String,
        duration_ms: u64,
        success: bool,
        event_count: u32,
    },
    /// 模型响应文本（用于重复检测）。
    ResponseText {
        turn: u32,
        text: String,
    },
    /// 错误发生。
    ErrorOccurred {
        error_type: String,
        message: String,
        turn: u32,
    },
    /// 上下文压力更新。
    PressureUpdated {
        pct: u32,
        msg_count: usize,
        max_messages: usize,
    },
    /// 引擎关闭——保存最终快照。
    Shutdown,
}

// ── 结构化诊断 ────────────────────────────────────────────

/// Roses/Buds/Thorns 诊断（来自 VIGIL）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Diagnosis {
    pub roses: Vec<String>,   // 稳定成功
    pub buds: Vec<String>,    // 新兴机会
    pub thorns: Vec<String>,  // 系统故障
    pub overall: String,      // ON_TRACK / LOOPING / DRIFTING / STUCK
}

// ── Neuro 观察者 ──────────────────────────────────────────

pub struct NeuroObserver {
    /// 接收事件的通道写入端（给 loop 用）
    pub tx: mpsc::UnboundedSender<NeuroEvent>,

    /// 后台任务句柄
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl NeuroObserver {
    /// 启动 Neuro 观察者。
    ///
    /// 返回 NeuroObserver（持有 tx 给主循环发事件，后台任务消费）。
    /// `db_conn` 是 libSQL 连接的 Arc（用于持久化快照）。
    /// `inner` 是现有的 MemNeuro（共享——保持 Neuro trait 兼容）。
    pub fn spawn(
        db_conn: Arc<TokioMutex<Connection>>,
        inner: Arc<crate::neuro_impl::MemNeuro>,
    ) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<NeuroEvent>();

        let handle = tokio::spawn(async move {
            let mut state = ObserverState::new(inner);

            while let Some(event) = rx.recv().await {
                state.process(event);

                // 每 10 轮持久化一次快照
                if state.turns_since_snapshot >= 10 {
                    state.persist(&db_conn).await;
                    state.turns_since_snapshot = 0;
                }
            }

            // 通道关闭 → 最终持久化
            state.persist(&db_conn).await;
        });

        NeuroObserver {
            tx,
            handle: Some(handle),
        }
    }

    /// 发送事件（主循环调用）。
    pub fn emit(&self, event: NeuroEvent) {
        let _ = self.tx.send(event);
    }
}

impl Drop for NeuroObserver {
    fn drop(&mut self) {
        // 通道关闭——后台任务会收到 None 并持久化
    }
}

// ── 内部状态 ──────────────────────────────────────────────

struct ObserverState {
    inner: Arc<crate::neuro_impl::MemNeuro>,
    session_start: Instant,
    turns_since_snapshot: u32,

    // 三层日志累积
    tool_success_streaks: std::collections::HashMap<String, u32>, // 工具 → 连续成功次数
    tool_fail_streaks: std::collections::HashMap<String, u32>,    // 工具 → 连续失败次数
    recent_pressures: Vec<u32>,                                     // 最近压力记录
}

impl ObserverState {
    fn new(inner: Arc<crate::neuro_impl::MemNeuro>) -> Self {
        ObserverState {
            inner,
            session_start: Instant::now(),
            turns_since_snapshot: 0,
            tool_success_streaks: std::collections::HashMap::new(),
            tool_fail_streaks: std::collections::HashMap::new(),
            recent_pressures: Vec::new(),
        }
    }

    fn process(&mut self, event: NeuroEvent) {
        match event {
            NeuroEvent::TurnCompleted { turn, tokens, tool_calls } => {
                self.inner.record_turn(turn, tokens, tool_calls);
                self.turns_since_snapshot += 1;
            }
            NeuroEvent::ToolExecuted { name, success, duration_ms } => {
                self.inner.record_tool_result(&name, success, duration_ms);
                if success {
                    *self.tool_success_streaks.entry(name.clone()).or_default() += 1;
                    self.tool_fail_streaks.insert(name, 0);
                } else {
                    *self.tool_fail_streaks.entry(name.clone()).or_default() += 1;
                    self.tool_success_streaks.insert(name, 0);
                }
            }
            NeuroEvent::ApiCalled { provider, duration_ms, success, event_count } => {
                self.inner.record_api_call(&provider, duration_ms, success, event_count);
            }
            NeuroEvent::ResponseText { turn, text } => {
                self.inner.record_response(turn, &text);
            }
            NeuroEvent::ErrorOccurred { error_type, message, turn } => {
                // 通过 EngineError 接口记录
                self.inner.record_error_str(&error_type, &message, turn);
            }
            NeuroEvent::PressureUpdated { pct, msg_count: _, max_messages: _ } => {
                self.inner.set_context_pressure(pct);
                self.recent_pressures.push(pct);
                if self.recent_pressures.len() > 20 {
                    self.recent_pressures.remove(0);
                }
            }
            NeuroEvent::Shutdown => {
                // 信号——持久化在 rx.recv() 返回 None 时触发
            }
        }
    }

    /// 生成结构化诊断。
    pub fn diagnose(&self) -> Diagnosis {
        let mut roses = Vec::new();
        let mut buds = Vec::new();
        let mut thorns = Vec::new();

        // Roses：连续成功 ≥ 5 次的工具
        for (tool, streak) in &self.tool_success_streaks {
            if *streak >= 5 {
                roses.push(format!("{tool}: 连续成功 {} 次", streak));
            } else if *streak >= 2 {
                buds.push(format!("{tool}: 开始稳定 ({} 次)", streak));
            }
        }

        // Thorns：连续失败 ≥ 3 次的工具
        for (tool, streak) in &self.tool_fail_streaks {
            if *streak >= 3 {
                thorns.push(format!("{tool}: 连续失败 {} 次", streak));
            }
        }

        // 重复检测
        if self.inner.check_repetition() {
            thorns.push("检测到响应重复——可能陷入循环".into());
        }

        let s = self.inner.status();
        if s.sleep_suggested {
            thorns.push(format!("睡眠建议触发——压力 {}%", s.context_pressure_pct));
        }

        // 总体状态
        let overall = if s.stuck_detected {
            "STUCK"
        } else if s.sleep_suggested && s.repetition_detected {
            "LOOPING"
        } else if s.sleep_suggested {
            "DRIFTING"
        } else if !thorns.is_empty() {
            "ON_TRACK (有警告)"
        } else {
            "ON_TRACK"
        };

        Diagnosis { roses, buds, thorns, overall: overall.into() }
    }

    /// 持久化快照到 libSQL。
    async fn persist(&self, db_conn: &Arc<TokioMutex<Connection>>) {
        let snapshot = serde_json::json!({
            "status": self.inner.status(),
            "diagnosis": self.diagnose(),
            "uptime_s": self.session_start.elapsed().as_secs(),
            "timestamp": schema::current_timestamp(),
        });

        let json = match serde_json::to_string(&snapshot) {
            Ok(j) => j,
            Err(_) => return,
        };

        let now = schema::current_timestamp();
        let conn = db_conn.lock().await;
        let _ = conn.execute(
            "INSERT INTO neuro_snapshots (snapshot_json, created_at) VALUES (?1, ?2)",
            rusqlite::params![json, now],
        );
    }
}
