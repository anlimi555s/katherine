// persistence/session.rs — Session JSONL 持久化。
// PikoClaw 思路：逐条追加 + 原子写入 + 关闭标记。

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::redact::redact_secrets;

/// Session 关闭原因。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Completed,
    UserStopped,
}

/// Session JSONL 中的一行。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "entry")]
pub enum SessionEntry {
    #[serde(rename = "start")]
    Start {
        t: u64,
        cwd: String,
        model: String,
    },
    #[serde(rename = "event")]
    Event {
        t: u64,
        #[serde(flatten)]
        payload: serde_json::Value,
    },
    #[serde(rename = "end")]
    End {
        t: u64,
        reason: CloseReason,
    },
}

/// Session 恢复信息。
#[derive(Debug, Clone)]
pub struct SessionRecovery {
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub entries: Vec<SessionEntry>,
    pub closed: bool,
    pub close_reason: Option<CloseReason>,
    /// 崩溃前的最后几个事件（供诊断显示）
    pub last_events: Vec<String>,
}

/// Session 持久化接口。
pub trait SessionStore: Send + Sync {
    /// 追加一行到当前 session。
    fn append(&self, entry: &SessionEntry);
    /// 写 Session 关闭标记。
    fn close(&self, reason: CloseReason);
    /// 检查是否有未闭合的 session（崩溃恢复）。
    fn check_recovery(&self) -> Option<SessionRecovery>;
    /// 列出所有 session。
    fn list_sessions(&self) -> Vec<String>;
    /// 加载指定 session。
    fn load_session(&self, id: &str) -> Option<Vec<SessionEntry>>;
}

/// 文件系统实现的 SessionStore。
/// 目录: {state_dir}/sessions/{ index.json + {session_id}.jsonl }
pub struct JsonlSessionStore {
    state_dir: PathBuf,
    current_file: std::sync::Mutex<Option<File>>,
    current_id: std::sync::Mutex<Option<String>>,
}

impl JsonlSessionStore {
    pub fn new(state_dir: &Path) -> Self {
        let sessions_dir = state_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).ok();

        JsonlSessionStore {
            state_dir: state_dir.to_path_buf(),
            current_file: std::sync::Mutex::new(None),
            current_id: std::sync::Mutex::new(None),
        }
    }

    /// 开始新 session。
    pub fn start_session(&self, cwd: &str, model: &str) {
        let now = now_secs();
        let session_id = format!("session-{}", now);

        let sessions_dir = self.state_dir.join("sessions");
        let path = sessions_dir.join(format!("{}.jsonl", &session_id));

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("Failed to open session file");

        let start = SessionEntry::Start {
            t: now,
            cwd: cwd.to_string(),
            model: model.to_string(),
        };
        writeln!(file, "{}", serde_json::to_string(&start).unwrap()).ok();
        file.flush().ok();

        // 更新 index
        let index_path = sessions_dir.join("index.json");
        let index = serde_json::json!({
            "latest": session_id,
            "cwd": cwd,
            "t": now,
        });
        fs::write(&index_path, serde_json::to_string_pretty(&index).unwrap()).ok();

        *self.current_file.lock().unwrap() = Some(file);
        *self.current_id.lock().unwrap() = Some(session_id);
    }

    /// 检查是否有未关闭的 session（崩溃恢复）。
    fn find_unclosed(&self) -> Option<(String, Vec<SessionEntry>)> {
        let sessions_dir = self.state_dir.join("sessions");
        if !sessions_dir.exists() {
            return None;
        }

        // 读 index.json 找到最近的 session
        let index_path = sessions_dir.join("index.json");
        if !index_path.exists() {
            return None;
        }

        let latest_id = fs::read_to_string(&index_path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["latest"].as_str().map(String::from));

        let id = match latest_id {
            Some(id) => id,
            None => return None,
        };

        let path = sessions_dir.join(format!("{}.jsonl", &id));
        if !path.exists() {
            return None;
        }

        let entries = read_jsonl(&path);
        let closed = entries
            .last()
            .map(|e| matches!(e, SessionEntry::End { .. }))
            .unwrap_or(false);

        if closed {
            None // 正常关闭
        } else {
            Some((id, entries))
        }
    }
}

impl SessionStore for JsonlSessionStore {
    fn append(&self, entry: &SessionEntry) {
        if let Some(ref mut file) = *self.current_file.lock().unwrap() {
            let line = serde_json::to_string(entry).unwrap_or_default();
            // 涂抹密钥再写盘——内存里保留明文，磁盘上不可见
            let safe = redact_secrets(&line);
            writeln!(file, "{}", safe).ok();
        }
    }

    fn close(&self, reason: CloseReason) {
        let end = SessionEntry::End {
            t: now_secs(),
            reason,
        };
        self.append(&end);
        if let Some(ref mut file) = *self.current_file.lock().unwrap() {
            file.flush().ok();
        }
        // 关闭文件句柄
        *self.current_file.lock().unwrap() = None;
        *self.current_id.lock().unwrap() = None;
    }

    fn check_recovery(&self) -> Option<SessionRecovery> {
        let (id, entries) = self.find_unclosed()?;

        let start_info = entries.first().and_then(|e| match e {
            SessionEntry::Start { cwd, model, .. } => Some((cwd.clone(), model.clone())),
            _ => None,
        });

        let (cwd, model) = start_info.unwrap_or_default();

        // 取最后 5 个事件做诊断摘要
        let last_events: Vec<String> = entries
            .iter()
            .rev()
            .take(5)
            .rev()
            .map(|e| match e {
                SessionEntry::Event { payload, .. } => {
                    let ev_type = payload["type"].as_str().unwrap_or("?");
                    match ev_type {
                        "tool_call" => {
                            format!("调用了 {} 工具", payload["name"].as_str().unwrap_or("?"))
                        }
                        "tool_result" => {
                            let ok = !payload["result"]["is_error"].as_bool().unwrap_or(true);
                            if ok {
                                "工具执行成功".into()
                            } else {
                                format!(
                                    "工具错误: {}",
                                    payload["result"]["content"]
                                        .as_str()
                                        .unwrap_or("?")
                                        .chars()
                                        .take(60)
                                        .collect::<String>()
                                )
                            }
                        }
                        "text" => {
                            let text = payload["text"].as_str().unwrap_or("");
                            format!("我说: {}", text.chars().take(50).collect::<String>())
                        }
                        "error" => {
                            format!("引擎错误: {}", payload["error"].as_str().unwrap_or("?"))
                        }
                        "turn_started" => {
                            format!("第 {} 轮开始", payload["turn"].as_u64().unwrap_or(0))
                        }
                        "done" => "对话结束".into(),
                        _ => format!("事件: {ev_type}"),
                    }
                }
                SessionEntry::End { reason, .. } => {
                    format!("会话关闭: {reason:?}")
                }
                SessionEntry::Start { cwd, model, .. } => {
                    format!("会话开始: {cwd} ({model})")
                }
            })
            .collect();

        Some(SessionRecovery {
            session_id: id,
            cwd,
            model,
            entries: entries.clone(),
            closed: false,
            close_reason: None,
            last_events,
        })
    }

    fn list_sessions(&self) -> Vec<String> {
        let sessions_dir = self.state_dir.join("sessions");
        if !sessions_dir.exists() {
            return Vec::new();
        }
        let mut ids: Vec<String> = fs::read_dir(&sessions_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false))
            .filter_map(|e| {
                e.path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(String::from)
            })
            .collect();
        ids.sort();
        ids.reverse(); // 最新的在前
        ids
    }

    fn load_session(&self, id: &str) -> Option<Vec<SessionEntry>> {
        let path = self.state_dir.join("sessions").join(format!("{}.jsonl", id));
        if path.exists() {
            Some(read_jsonl(&path))
        } else {
            None
        }
    }
}

// ── Helpers ───────────────────────────────────────────────

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_jsonl(path: &Path) -> Vec<SessionEntry> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    BufReader::new(file)
        .lines()
        .filter_map(|l| l.ok())
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<SessionEntry>(&l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn session_start_and_close() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlSessionStore::new(tmp.path());
        store.start_session("/test", "deepseek-v4");

        // 追加事件
        store.append(&SessionEntry::Event {
            t: now_secs(),
            payload: serde_json::json!({"type": "text", "text": "hello"}),
        });

        // 正常关闭
        store.close(CloseReason::Completed);

        // 检查恢复——应该没有未关闭的 session
        assert!(store.check_recovery().is_none());
    }

    #[test]
    fn crash_recovery_detected() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlSessionStore::new(tmp.path());
        store.start_session("/test", "deepseek-v4");

        store.append(&SessionEntry::Event {
            t: now_secs(),
            payload: serde_json::json!({"type": "tool_call", "name": "Read"}),
        });
        store.append(&SessionEntry::Event {
            t: now_secs(),
            payload: serde_json::json!({"type": "tool_result", "result": {"is_error": false, "content": "ok"}}),
        });

        // 不调 close——模拟崩溃
        // 手动 flush 保证数据落到磁盘（close 会做，崩溃前模拟 flush）
        if let Some(ref mut f) = *store.current_file.lock().unwrap() {
            f.flush().ok();
        }

        // 释放文件句柄（模拟进程退出）
        *store.current_file.lock().unwrap() = None;

        // 新实例检查
        let store2 = JsonlSessionStore::new(tmp.path());
        let recovery = store2.check_recovery();
        assert!(recovery.is_some());
        let r = recovery.unwrap();
        assert!(!r.closed);
        assert!(!r.last_events.is_empty());
        // 最后事件应该包含 tool_result
        assert!(r.last_events.iter().any(|e| e.contains("工具")));
    }

    #[test]
    fn user_stopped_no_recovery() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlSessionStore::new(tmp.path());
        store.start_session("/test", "deepseek-v4");
        store.close(CloseReason::UserStopped);

        let store2 = JsonlSessionStore::new(tmp.path());
        assert!(store2.check_recovery().is_none());
    }
}
