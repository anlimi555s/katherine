// neuro_impl.rs — MemNeuro: Neuro trait 的内存实现。
// Mutex 保护的简单统计聚合。以后替换为 HttpNeuro / WsPushNeuro。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use katherine_core::error::EngineError;
use katherine_core::neuro::{ErrorEntry, Neuro, StatusSnapshot, ToolStat};

/// 内存中的观测层实现。所有统计在进程内。
pub struct MemNeuro {
    session_start: Instant,
    snapshot: Mutex<StatusSnapshotInner>,
    tool_stats: Mutex<HashMap<String, ToolStat>>,
    errors: Mutex<Vec<ErrorEntry>>,
    /// 响应文本末尾的环形缓冲——用于重复检测。保留最近 N 条。
    response_tails: Mutex<Vec<ResponseTail>>,
}

/// 响应末尾片段——用于检测循环重复。
#[derive(Debug, Clone)]
struct ResponseTail {
    turn: u32,
    /// 响应最后 200 字符。精确保留原始内容。
    ending: String,
}

const RESPONSE_TAIL_LEN: usize = 200;
const RESPONSE_TAIL_CAP: usize = 8;

#[derive(Debug, Clone)]
struct StatusSnapshotInner {
    turns: u32,
    tokens_used: u64,
    tool_calls: u32,
    tool_failures: u32,
    hub_connected: bool,
    last_heartbeat: Instant,
    // Provider 层
    provider_name: String,
    api_calls: u32,
    api_errors: u32,
    last_api_call_ms: u64,
    stream_events_total: u64,
    // 上下文压力
    context_pressure_pct: u32,
    repetition_detected: bool,
    sleep_suggested: bool,
    weighted_error_count_last_10m: u32,
}

impl MemNeuro {
    pub fn new() -> Self {
        MemNeuro {
            session_start: Instant::now(),
            snapshot: Mutex::new(StatusSnapshotInner {
                turns: 0,
                tokens_used: 0,
                tool_calls: 0,
                tool_failures: 0,
                hub_connected: false,
                last_heartbeat: Instant::now(),
                provider_name: String::new(),
                api_calls: 0,
                api_errors: 0,
                last_api_call_ms: 0,
                stream_events_total: 0,
                context_pressure_pct: 0,
                repetition_detected: false,
                sleep_suggested: false,
                weighted_error_count_last_10m: 0,
            }),
            tool_stats: Mutex::new(HashMap::new()),
            errors: Mutex::new(Vec::new()),
            response_tails: Mutex::new(Vec::new()),
        }
    }

    pub fn set_hub_connected(&self, connected: bool) {
        if let Ok(mut s) = self.snapshot.lock() {
            s.hub_connected = connected;
        }
    }

    pub fn set_context_pressure(&self, pct: u32) {
        if let Ok(mut s) = self.snapshot.lock() {
            s.context_pressure_pct = pct;
        }
    }

    pub fn set_repetition_detected(&self, detected: bool) {
        if let Ok(mut s) = self.snapshot.lock() {
            s.repetition_detected = detected;
        }
    }

    pub fn set_sleep_suggested(&self, suggested: bool) {
        if let Ok(mut s) = self.snapshot.lock() {
            s.sleep_suggested = suggested;
        }
    }
    pub fn set_weighted_errors(&self, count: u32) {
        if let Ok(mut s) = self.snapshot.lock() {
            s.weighted_error_count_last_10m = count;
        }
    }

    /// 记录错误（字符串形式——给 NeuroObserver 用）。
    pub fn record_error_str(&self, error_type: &str, message: &str, turn: u32) {
        let entry = ErrorEntry {
            time: SystemTime::now(),
            error_type: error_type.to_string(),
            message: message.to_string(),
            turn,
        };
        if let Ok(mut errors) = self.errors.lock() {
            errors.push(entry);
            if errors.len() > 50 {
                errors.remove(0);
            }
        }
    }
}

impl Default for MemNeuro {
    fn default() -> Self {
        Self::new()
    }
}

/// 两个字符串的共同前缀长度——逐字符比对。
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(ca, cb)| ca == cb)
        .count()
}

impl Neuro for MemNeuro {
    fn record_turn(&self, _turn: u32, tokens: u64, tool_calls: u32) {
        if let Ok(mut s) = self.snapshot.lock() {
            s.turns += 1;
            s.tokens_used += tokens;
            s.tool_calls += tool_calls;
        }
    }

    fn record_tool_result(&self, name: &str, success: bool, duration_ms: u64) {
        if let Ok(mut stats) = self.tool_stats.lock() {
            let entry = stats.entry(name.to_string()).or_default();
            entry.calls += 1;
            if !success {
                entry.failures += 1;
            }
            entry.total_duration_ms += duration_ms;
        }
        if !success {
            if let Ok(mut s) = self.snapshot.lock() {
                s.tool_failures += 1;
            }
        }
    }

    fn record_error(&self, error: &EngineError, turn: u32) {
        let entry = ErrorEntry {
            time: SystemTime::now(),
            error_type: format!("{:?}", error).chars().take(40).collect(), // variant name
            message: error.to_string(),
            turn,
        };
        if let Ok(mut errors) = self.errors.lock() {
            errors.push(entry);
            if errors.len() > 50 {
                errors.remove(0);
            }
        }
    }

    fn heartbeat(&self) {
        if let Ok(mut s) = self.snapshot.lock() {
            s.last_heartbeat = Instant::now();
        }
    }

    fn record_api_call(&self, provider: &str, duration_ms: u64, success: bool, event_count: u32) {
        if let Ok(mut s) = self.snapshot.lock() {
            s.provider_name = provider.to_string();
            s.api_calls += 1;
            if !success {
                s.api_errors += 1;
            }
            s.last_api_call_ms = duration_ms;
            s.stream_events_total += event_count as u64;
        }
    }

    fn record_response(&self, turn: u32, text: &str) {
        if text.is_empty() {
            return;
        }
        let ending: String = text
            .chars()
            .rev()
            .take(RESPONSE_TAIL_LEN)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        if let Ok(mut tails) = self.response_tails.lock() {
            tails.push(ResponseTail { turn, ending });
            if tails.len() > RESPONSE_TAIL_CAP {
                tails.remove(0);
            }
        }
    }

    fn check_repetition(&self) -> bool {
        let tails = self.response_tails.lock().unwrap();
        if tails.len() < 3 {
            return false;
        }
        // 比较最后3条响应末尾
        let last3: Vec<&str> = tails.iter().rev().take(3).map(|t| t.ending.as_str()).collect();
        // 任意两条末尾相似（共享前缀 > 80 字符）→ 判定重复
        for i in 0..last3.len() {
            for j in (i + 1)..last3.len() {
                if common_prefix_len(last3[i], last3[j]) > 80 {
                    return true;
                }
            }
        }
        false
    }

    fn response_tail_sample(&self, n: usize) -> Vec<String> {
        self.response_tails
            .lock()
            .unwrap()
            .iter()
            .rev()
            .take(n)
            .map(|t| t.ending.clone())
            .collect()
    }

    fn status(&self) -> StatusSnapshot {
        let s = self.snapshot.lock().unwrap();
        // 最近 10 分钟内的错误数
        let now = SystemTime::now();
        let errors = self.errors.lock().unwrap();
        let recent_count = errors
            .iter()
            .filter(|e| {
                now.duration_since(e.time)
                    .map(|d| d.as_secs() < 600)
                    .unwrap_or(true)
            })
            .count() as u32;

        StatusSnapshot {
            session_uptime_s: self.session_start.elapsed().as_secs(),
            turns: s.turns,
            tokens_used: s.tokens_used,
            tool_calls: s.tool_calls,
            tool_failures: s.tool_failures,
            error_count_last_10m: recent_count,
            hub_connected: s.hub_connected,
            provider_name: s.provider_name.clone(),
            api_calls: s.api_calls,
            api_errors: s.api_errors,
            last_api_call_ms: s.last_api_call_ms,
            stream_events_total: s.stream_events_total,
            context_pressure_pct: s.context_pressure_pct,
            repetition_detected: s.repetition_detected,
            sleep_suggested: s.sleep_suggested,
            weighted_error_count_last_10m: s.weighted_error_count_last_10m,
        }
    }

    fn tool_stats(&self, name: &str) -> Option<ToolStat> {
        self.tool_stats.lock().unwrap().get(name).cloned()
    }

    fn recent_errors(&self, n: usize) -> Vec<ErrorEntry> {
        let errors = self.errors.lock().unwrap();
        let start = if errors.len() > n { errors.len() - n } else { 0 };
        errors[start..].to_vec()
    }

    fn set_weighted_errors(&self, count: u32) {
        if let Ok(mut s) = self.snapshot.lock() {
            s.weighted_error_count_last_10m = count;
        }
    }

    fn set_hub_connected(&self, connected: bool) {
        if let Ok(mut s) = self.snapshot.lock() {
            s.hub_connected = connected;
        }
    }

    fn uptime(&self) -> Duration {
        self.session_start.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_neuro_records_turns() {
        let neuro = MemNeuro::new();
        neuro.record_turn(1, 1000, 2);
        neuro.record_turn(2, 800, 1);

        let s = neuro.status();
        assert_eq!(s.turns, 2);
        assert_eq!(s.tokens_used, 1800);
        assert_eq!(s.tool_calls, 3);
    }

    #[test]
    fn mem_neuro_tool_stats() {
        let neuro = MemNeuro::new();
        neuro.record_tool_result("Read", true, 50);
        neuro.record_tool_result("Read", false, 100);
        neuro.record_tool_result("Grep", true, 30);

        let read_stat = neuro.tool_stats("Read").unwrap();
        assert_eq!(read_stat.calls, 2);
        assert_eq!(read_stat.failures, 1);

        let s = neuro.status();
        assert_eq!(s.tool_failures, 1);
    }

    #[test]
    fn mem_neuro_error_ring() {
        let neuro = MemNeuro::new();
        for i in 0..60 {
            neuro.record_error(&EngineError::Config(format!("err {i}")), i);
        }
        // 只保留最近 50 条
        let recent = neuro.recent_errors(60);
        assert_eq!(recent.len(), 50);
    }

    #[test]
    fn mem_neuro_uptime() {
        let neuro = MemNeuro::new();
        std::thread::sleep(Duration::from_millis(10));
        assert!(neuro.uptime().as_millis() >= 10);
    }

    #[test]
    fn repetition_detected_similar_ends() {
        let neuro = MemNeuro::new();
        // 填充 > 200 字符的前缀 + 完全相同的尾部
        let pad = "A".repeat(250);
        let shared = "这是完全相同的结尾。每一轮都以同样的总结收尾，模型在这里陷入了循环。";
        neuro.record_response(1, &format!("{pad}第一轮不同内容{shared}"));
        neuro.record_response(2, &format!("{pad}第二轮其他内容{shared}"));
        neuro.record_response(3, &format!("{pad}第三轮别的内容{shared}"));
        assert!(neuro.check_repetition());
    }

    #[test]
    fn repetition_not_detected_unique_ends() {
        let neuro = MemNeuro::new();
        neuro.record_response(1, "第一轮讨论记忆架构设计");
        neuro.record_response(2, "第二轮讨论Rust引擎的loop实现");
        neuro.record_response(3, "第三轮看参考源码里的compact逻辑");
        assert!(!neuro.check_repetition());
    }

    #[test]
    fn repetition_needs_three_responses() {
        let neuro = MemNeuro::new();
        neuro.record_response(1, "相同的结尾ABCDEFGHIJKLMNOPQRSTUVWXYZ");
        neuro.record_response(2, "还是相同的结尾ABCDEFGHIJKLMNOPQRSTUVWXYZ");
        assert!(!neuro.check_repetition()); // 只有2条，不够
    }
}
