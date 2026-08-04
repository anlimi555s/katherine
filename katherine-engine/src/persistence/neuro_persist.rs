// persistence/neuro_persist.rs — Neuro JSONL 持久化。
// heartbit-ghost 思路：O_APPEND + 按天分文件 + 启动重放。

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use katherine_core::error::EngineError;
use katherine_core::neuro::{Neuro, StatusSnapshot, ToolStat, ErrorEntry};

use super::redact::redact_secrets;
use crate::neuro_impl::MemNeuro;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 从文件名提取天数（排序用）。
fn filename_to_day(filename: &str) -> Option<u32> {
    let stem = filename.strip_prefix("neuro-")?.strip_suffix(".jsonl")?;
    stem.parse().ok()
}

/// Neuro JSONL 中的一行。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
enum NeuroEntry {
    #[serde(rename = "turn")]
    Turn {
        t: u64,
        turn: u32,
        tokens: u64,
        tools: u32,
    },
    #[serde(rename = "tool")]
    Tool {
        t: u64,
        name: String,
        ok: bool,
        ms: u64,
    },
    #[serde(rename = "error")]
    ErrorEntry {
        t: u64,
        error: String,
        #[serde(default)]
        variant: String,
        turn: u32,
    },
    #[serde(rename = "heartbeat")]
    Heartbeat { t: u64 },
}

/// Neuro 持久化包装。包装 MemNeuro，内部方法追加写 JSONL。
pub struct PersistNeuro {
    inner: MemNeuro,
    neuro_dir: PathBuf,
    current_file: Mutex<Option<File>>,
    current_day: Mutex<u32>,
}

impl PersistNeuro {
    /// 创建并重放历史数据。
    pub fn open(state_dir: &Path) -> Self {
        let neuro_dir = state_dir.join("neuro");
        fs::create_dir_all(&neuro_dir).ok();

        let inner = MemNeuro::new();

        // 重放最近 3 天的日志
        let today = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32
            / 86400;

        for day in (today.saturating_sub(2))..=today {
            let filename = format!("neuro-{:05}.jsonl", day);
            let path = neuro_dir.join(&filename);
            if path.exists() {
                replay_file(&inner, &path);
            }
        }

        let neuro = PersistNeuro {
            inner,
            neuro_dir,
            current_file: Mutex::new(None),
            current_day: Mutex::new(today),
        };

        // 压缩旧文件 + 清理
        neuro.maintain();

        // 打开今天的文件
        neuro.ensure_file_for_day(today);

        neuro
    }

    fn ensure_file_for_day(&self, day: u32) {
        let filename = format!("neuro-{:05}.jsonl", day);
        let path = self.neuro_dir.join(&filename);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("Failed to open neuro file");
        *self.current_file.lock().unwrap() = Some(file);
        *self.current_day.lock().unwrap() = day;
    }

    fn check_rotate(&self) {
        let today = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32
            / 86400;

        let current = *self.current_day.lock().unwrap();
        if today != current {
            self.ensure_file_for_day(today);
        }
    }

    fn append(&self, entry: &NeuroEntry) {
        self.check_rotate();
        if let Some(ref mut file) = *self.current_file.lock().unwrap() {
            let line = serde_json::to_string(entry).unwrap_or_default();
            let safe = redact_secrets(&line);
            writeln!(file, "{}", safe).ok();
        }
    }

    /// 维护：压缩旧文件、删除过期文件。
    fn maintain(&self) {
        let today = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32
            / 86400;

        let entries: Vec<(u32, PathBuf)> = fs::read_dir(&self.neuro_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                filename_to_day(&name).map(|day| (day, e.path()))
            })
            .collect();

        for (day, path) in &entries {
            if *day < today.saturating_sub(365) {
                // 超过 365 天——删除
                fs::remove_file(path).ok();
            } else if *day < today.saturating_sub(2) {
                // 超过 2 天但不到 365——压缩（如果有 gzip 支持）
                // Phase 7 先跳过 gzip——用 flate2 以后再装
                // 标记：TODO: gzip old neuro files
            }
        }
    }

    /// 获取内部 MemNeuro 的引用（测试用）。
    #[allow(dead_code)]
    pub fn inner(&self) -> &MemNeuro {
        &self.inner
    }
}

impl Neuro for PersistNeuro {
    fn record_turn(&self, turn: u32, tokens: u64, tools: u32) {
        self.inner.record_turn(turn, tokens, tools);
        self.append(&NeuroEntry::Turn {
            t: now_secs(),
            turn,
            tokens,
            tools,
        });
    }

    fn record_tool_result(&self, name: &str, success: bool, duration_ms: u64) {
        self.inner.record_tool_result(name, success, duration_ms);
        self.append(&NeuroEntry::Tool {
            t: now_secs(),
            name: name.to_string(),
            ok: success,
            ms: duration_ms,
        });
    }

    fn record_error(&self, error: &EngineError, turn: u32) {
        self.inner.record_error(error, turn);
        // 提取错误变体名——保留类型信息用于重放重建
        let variant = {
            let debug = format!("{:?}", error);
            debug
                .split(|c: char| c == '(' || c == '{' || c == ' ')
                .next()
                .unwrap_or("Unknown")
                .to_string()
        };
        self.append(&NeuroEntry::ErrorEntry {
            t: now_secs(),
            error: error.to_string(),
            variant,
            turn,
        });
    }

    fn heartbeat(&self) {
        self.inner.heartbeat();
        self.append(&NeuroEntry::Heartbeat { t: now_secs() });
    }

    fn status(&self) -> StatusSnapshot {
        self.inner.status()
    }

    fn record_response(&self, turn: u32, text: &str) {
        self.inner.record_response(turn, text)
    }

    fn check_repetition(&self) -> bool {
        self.inner.check_repetition()
    }

    fn response_tail_sample(&self, n: usize) -> Vec<String> {
        self.inner.response_tail_sample(n)
    }

    fn tool_stats(&self, name: &str) -> Option<ToolStat> {
        self.inner.tool_stats(name)
    }

    fn recent_errors(&self, n: usize) -> Vec<ErrorEntry> {
        self.inner.recent_errors(n)
    }

    fn uptime(&self) -> std::time::Duration {
        self.inner.uptime()
    }

    fn record_api_call(&self, provider: &str, duration_ms: u64, success: bool, event_count: u32) {
        self.inner.record_api_call(provider, duration_ms, success, event_count);
        // Neuro JSONL 不记每条 API 调——太频繁。只记 error 和 turn。
    }
}

/// 重放一个 JSONL 文件到 MemNeuro。
fn replay_file(neuro: &MemNeuro, path: &Path) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };

    for line in BufReader::new(file).lines().filter_map(|l| l.ok()) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<NeuroEntry>(&line) {
            match entry {
                NeuroEntry::Turn { turn, tokens, tools, .. } => {
                    neuro.record_turn(turn, tokens, tools);
                }
                NeuroEntry::Tool { name, ok, ms, .. } => {
                    neuro.record_tool_result(&name, ok, ms);
                }
                NeuroEntry::ErrorEntry { error, variant, turn, .. } => {
                    let reconstructed = match variant.as_str() {
                        "HubUnreachable" => EngineError::HubUnreachable(error),
                        "HubTimeout" => EngineError::HubTimeout,
                        "EmptyResponse" => EngineError::EmptyResponse,
                        "MaxTurnsReached" => {
                            let n = error.split_whitespace().last()
                                .and_then(|s| s.parse().ok()).unwrap_or(0);
                            EngineError::MaxTurnsReached(n)
                        }
                        "ProviderAuth" => EngineError::ProviderAuth(error),
                        "ProviderRateLimited" => EngineError::ProviderRateLimited { retry_after_s: 0 },
                        "ProviderOverloaded" => EngineError::ProviderOverloaded,
                        "ProviderServer" => EngineError::ProviderServer(500, error),
                        "ProviderStreamInterrupted" => EngineError::ProviderStreamInterrupted(error),
                        "ToolNotFound" => EngineError::ToolNotFound(error),
                        "ToolExecutionFailed" => EngineError::ToolExecutionFailed {
                            name: "?".into(),
                            message: error,
                        },
                        "ToolPermissionDenied" => EngineError::ToolPermissionDenied {
                            name: error,
                        },
                        "ToolInputValidation" => EngineError::ToolInputValidation {
                            name: "?".into(),
                            errors: vec![error],
                        },
                        _ => EngineError::Config(error), // 兼容旧数据——variant 为空
                    };
                    neuro.record_error(&reconstructed, turn);
                }
                NeuroEntry::Heartbeat { .. } => {
                    neuro.heartbeat();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn neuro_persist_and_replay() {
        let tmp = TempDir::new().unwrap();

        {
            // 写入
            let neuro = PersistNeuro::open(tmp.path());
            neuro.record_turn(1, 1000, 2);
            neuro.record_turn(2, 800, 1);
            neuro.record_tool_result("Read", true, 50);
            neuro.record_error(&EngineError::Config("test error".into()), 3);
        }

        {
            // 重放
            let neuro = PersistNeuro::open(tmp.path());
            let s = neuro.status();
            assert_eq!(s.turns, 2);
            assert_eq!(s.tokens_used, 1800);
            assert_eq!(s.tool_calls, 3);
            assert_eq!(s.tool_failures, 0);
        }
    }

    #[test]
    fn neuro_persist_tool_failures_counted() {
        let tmp = TempDir::new().unwrap();

        {
            let neuro = PersistNeuro::open(tmp.path());
            neuro.record_tool_result("Bash", false, 100);
            neuro.record_tool_result("Bash", true, 50);
        }

        {
            let neuro = PersistNeuro::open(tmp.path());
            let s = neuro.status();
            assert_eq!(s.tool_failures, 1);
            let stat = neuro.tool_stats("Bash").unwrap();
            assert_eq!(stat.calls, 2);
            assert_eq!(stat.failures, 1);
        }
    }
}
