// memory/libsql_store.rs — LibSqlHub：基于 rusqlite 的 Hub trait 实现。
// 直接连接 katherine.db，无 HTTP，无外部服务。
// 与桌面设计稿 §六 保持一致。

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Connection;
use tokio::sync::Mutex as TokioMutex;

use katherine_core::error::EngineError;
use katherine_core::hub::{BootData, Hub, HubState};

use crate::memory::schema;
use crate::memory::retrieval;

/// 把 rusqlite::Error 转成 EngineError::Io。
fn sql_err(e: rusqlite::Error) -> EngineError {
    EngineError::Io(Arc::new(std::io::Error::new(std::io::ErrorKind::Other, e)))
}

/// 基于 libSQL / SQLite 的 Hub 实现。
pub struct LibSqlHub {
    conn: Arc<TokioMutex<Connection>>,
    db_path: PathBuf,
}

impl LibSqlHub {
    /// 打开（或创建）数据库文件，建表。
    pub fn new(db_path: PathBuf) -> Result<Self, EngineError> {
        Self::new_inner(db_path).map(|(store, _conn)| store)
    }

    /// 打开数据库，同时返回 Arc<Connection>（给 NeuroObserver 共享）。
    pub fn new_with_conn(db_path: PathBuf) -> Result<(Self, Arc<TokioMutex<Connection>>), EngineError> {
        Self::new_inner(db_path)
    }

    fn new_inner(db_path: PathBuf) -> Result<(Self, Arc<TokioMutex<Connection>>), EngineError> {
        // 确保父目录存在
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| EngineError::Io(Arc::new(e)))?;
        }

        let conn = Connection::open(&db_path)
            .map_err(sql_err)?;

        // 启用 WAL 模式——读不阻塞写
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(sql_err)?;

        schema::create_tables(&conn)
            .map_err(sql_err)?;
        schema::create_triggers(&conn)
            .map_err(sql_err)?;

        let conn = Arc::new(TokioMutex::new(conn));
        Ok((
            LibSqlHub {
                conn: conn.clone(),
                db_path,
            },
            conn,
        ))
    }

    /// 启动时整合——基于 supersedes 边的级联降权 + 低分归档。
    /// 三步：① supersedes 边指向的旧事件降权 ② 极低分事件归档 ③ 统计衰效事件。
    /// 返回报告。在主循环的 setup 阶段调用。
    pub async fn consolidate(&self) -> ConsolidationReport {
        let conn = self.conn.lock().await;
        let total: usize = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap_or(0);

        if total == 0 {
            return ConsolidationReport {
                total: 0,
                superseded: 0,
                decayed: 0,
                archived: 0,
            };
        }

        // 第一步：找出被 supersedes 边指向的事件，降权 50%
        // supersedes 方向：新记忆 → 旧记忆。被指向的旧记忆内容已被取代。
        let superseded = conn
            .execute(
                "UPDATE events
                 SET importance = MAX(ROUND(importance * 0.5, 4), 0.01)
                 WHERE id IN (SELECT target_id FROM edges WHERE edge_type = 'supersedes')
                 AND importance > 0.01",
                [],
            )
            .unwrap_or(0);

        // 第二步：归档极低分事件（importance < 0.001 且可衰减）
        // identity_anchor 永不归档（decay_curve = 'none'）
        let archived = conn
            .execute(
                "UPDATE events SET importance = ROUND(importance * 0.001, 6)
                 WHERE importance < 0.001
                 AND decay_curve != 'none'
                 AND source != 'identity_anchor'",
                [],
            )
            .unwrap_or(0);

        // 第三步：统计因低分而在检索中实质上失效的事件
        let decayed: usize = conn
            .query_row(
                "SELECT COUNT(*) FROM events
                 WHERE importance < 0.01 AND decay_curve != 'none'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        ConsolidationReport {
            total,
            superseded,
            decayed,
            archived,
        }
    }
}

/// 整合报告
#[derive(Debug, Clone)]
pub struct ConsolidationReport {
    pub total: usize,
    /// 被 supersedes 边降权的事件数
    pub superseded: usize,
    /// importance < 0.01 实质上失效的事件数
    pub decayed: usize,
    /// 被归档的事件数（importance × 0.001）
    pub archived: usize,
}

#[async_trait]
impl Hub for LibSqlHub {
    async fn boot(&self) -> Result<BootData, EngineError> {
        let conn = self.conn.lock().await;

        // 加载状态
        let state = {
            let mut stmt = conn
                .prepare("SELECT value FROM runtime_state WHERE key = 'state'")
                .ok();
            stmt.as_mut()
                .and_then(|s| s.query_row([], |r| r.get::<_, String>(0)).ok())
                .and_then(|json| serde_json::from_str::<HubState>(&json).ok())
        };

        // 加载最近记忆
        let mut stmt = conn
            .prepare("SELECT content FROM events ORDER BY created_at DESC LIMIT 10")
            .map_err(sql_err)?;

        let memories: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .map_err(sql_err)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(BootData { state, memories })
    }

    async fn health(&self) -> bool {
        // 本地存储——永不断连
        true
    }

    async fn mark_memory(
        &self,
        content: &str,
        importance: f32,
        source: &str,
    ) -> Result<(), EngineError> {
        if content.is_empty() {
            return Ok(());
        }

        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        let id = format!("mem-{:016x}", hasher.finish());

        let decay_curve = crate::memory::decay::DecayCurve::from_source(source, importance);
        let now = schema::current_timestamp();

        let conn = self.conn.lock().await;

        // 纠正/决策 → 检测矛盾，标记旧记忆 superseded
        if source == "selena_correction" || source == "my_decision" {
            let similar = retrieval::search(&conn, content, 3, schema::now_epoch_secs())
                .unwrap_or_default();

            conn.execute(
                "INSERT INTO events (id, content, source, importance, decay_curve, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, content, source, importance, decay_curve.as_str(), now],
            )
            .map_err(sql_err)?;

            for old_content in &similar {
                if let Ok(old_id) = conn.query_row(
                    "SELECT id FROM events WHERE content = ?1",
                    [old_content],
                    |r| r.get::<_, String>(0),
                ) {
                    let _ = conn.execute(
                        "INSERT OR IGNORE INTO edges (source_id, target_id, edge_type, created_at) \
                         VALUES (?1, ?2, 'supersedes', ?3)",
                        rusqlite::params![id, old_id, now],
                    );
                }
            }
        } else {
            conn.execute(
                "INSERT INTO events (id, content, source, importance, decay_curve, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, content, source, importance, decay_curve.as_str(), now],
            )
            .map_err(sql_err)?;
        }

        // ── 相似链拉——对所有新记忆，搜相似记忆建 relates_to 边 ──
        let similar = retrieval::search(&conn, content, 5, schema::now_epoch_secs())
            .unwrap_or_default();
        for old_content in &similar {
            if let Ok(old_id) = conn.query_row(
                "SELECT id FROM events WHERE content = ?1 AND id != ?2",
                [old_content.as_str(), id.as_str()],
                |r| r.get::<_, String>(0),
            ) {
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO edges (source_id, target_id, edge_type, created_at) \
                     VALUES (?1, ?2, 'relates_to', ?3)",
                    rusqlite::params![id, old_id, now],
                );
            }
        }

        Ok(())
    }

    async fn recall(&self, query: &str, limit: u32) -> Result<Vec<String>, EngineError> {
        let conn = self.conn.lock().await;
        let now = schema::now_epoch_secs();

        let results = retrieval::search(&conn, query, limit, now)
            .map_err(sql_err)?;

        // 更新检索统计
        let now_ts = schema::current_timestamp();
        for content in &results {
            let _ = conn.execute(
                "UPDATE events SET retrieval_count = retrieval_count + 1, last_retrieved = ?1 \
                 WHERE content = ?2",
                rusqlite::params![now_ts, content],
            );
        }

        // ── 边遍历：每个结果带上它的关联记忆 ──
        let mut enriched = Vec::new();
        for content in &results {
            let mut entry = content.clone();
            // 找这个记忆的相关边
            if let Ok(id) = conn.query_row(
                "SELECT id FROM events WHERE content = ?1",
                [content],
                |r| r.get::<_, String>(0),
            ) {
                // superseded_by 链——这个记忆替代了什么
                if let Ok(old) = conn.query_row(
                    "SELECT e.content FROM edges ed JOIN events e ON ed.target_id = e.id \
                     WHERE ed.source_id = ?1 AND ed.edge_type = 'supersedes' LIMIT 1",
                    [&id],
                    |r| r.get::<_, String>(0),
                ) {
                    entry.push_str("\n  ⤷ 替代了: ");
                    entry.push_str(&old.chars().take(100).collect::<String>());
                }
                // relates_to 链——关联记忆（最多 2 条）
                let mut related = conn.prepare(
                    "SELECT e.content FROM edges ed JOIN events e ON ed.target_id = e.id \
                     WHERE ed.source_id = ?1 AND ed.edge_type = 'relates_to' LIMIT 2"
                ).ok();
                if let Some(ref mut stmt) = related {
                    let rels: Vec<String> = stmt.query_map([&id], |r| r.get::<_, String>(0))
                        .ok()
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                        .unwrap_or_default();
                    for rel in &rels {
                        entry.push_str("\n  ⤿ 关联: ");
                        entry.push_str(&rel.chars().take(100).collect::<String>());
                    }
                }
            }
            enriched.push(entry);
        }

        Ok(enriched)
    }

    async fn save_state(
        &self,
        threads: &[String],
        mood: &str,
        risk: &str,
    ) -> Result<(), EngineError> {
        let state = HubState {
            updated: schema::current_timestamp(),
            mood: mood.to_string(),
            overload_risk: risk.to_string(),
            open_threads: threads.to_vec(),
            last_sleep: schema::current_timestamp(),
        };

        let json = serde_json::to_string(&state)
            .map_err(|e| EngineError::Io(Arc::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))))?;

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO runtime_state (key, value, updated_at) VALUES ('state', ?1, ?2)",
            rusqlite::params![json, schema::current_timestamp()],
        )
        .map_err(sql_err)?;

        Ok(())
    }
}
