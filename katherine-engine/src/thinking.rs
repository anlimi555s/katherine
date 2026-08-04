// thinking.rs — 认知档案：thinking token 持久化。
// 独立于记忆系统。不进 events 表、不进 FTS5、不被 recall 检索。
// O_APPEND JSONL，每会话一个文件。
//
// 论文基础: Cognitive Companion (2604.13759), DS-MCM (2601.23188), Cognitive Core (2604.10658)

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::Serialize;

/// 一轮 thinking 的完整记录。
#[derive(Debug, Clone, Serialize)]
pub struct ThinkingRecord {
    pub turn: u32,
    pub timestamp: String,
    pub thinking: String,
    pub thinking_len: usize,
    pub importance: f32,
    pub had_correction: bool,
    pub tools_called: Vec<String>,
    pub recall_hits: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_thinking_id: Option<String>,
}

/// 计算 thinking 重要性。
///
/// base 0.4 (默认 — 思考档案需要观察窗口)
/// + 0.2 如果本轮有工具调用（thinking → action）
/// + 0.3 如果本轮有纠正（thinking 被 reshape）
/// 上限 0.9。
pub fn compute_thinking_importance(
    tools_called: &[String],
    had_correction: bool,
) -> f32 {
    let mut imp: f32 = 0.4;
    if !tools_called.is_empty() {
        imp += 0.2;
    }
    if had_correction {
        imp += 0.3;
    }
    imp.min(0.9)
}

/// 追加一条 thinking 记录到会话 JSONL 文件。
///
/// 文件路径: {thinking_dir}/session-{YYYY-MM-DD}.jsonl
/// 目录不存在时自动创建。写入失败静默忽略（不中断 loop）。
pub fn append_thinking(thinking_dir: &PathBuf, record: &ThinkingRecord) {
    // 确保目录存在
    if let Err(e) = fs::create_dir_all(thinking_dir) {
        eprintln!("[thinking] Cannot create dir {:?}: {e}", thinking_dir);
        return;
    }

    // 生成文件名
    let filename = format!("session-{}.jsonl", &record.timestamp[..10]);
    let path = thinking_dir.join(filename);

    // 序列化
    let json = match serde_json::to_string(record) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("[thinking] Serialize error: {e}");
            return;
        }
    };

    // O_APPEND
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            let _ = writeln!(f, "{json}");
        }
        Err(e) => {
            eprintln!("[thinking] Cannot write {:?}: {e}", path);
        }
    }
}

/// 当前时间戳
pub fn now_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let days = secs / 86400;
    let tod = secs % 86400;
    let h = tod / 3600;
    let m = (tod % 3600) / 60;
    let s = tod % 60;

    let mut y = 1970i64;
    let mut d = days;
    loop {
        let diy = if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 366 } else { 365 };
        if d < diy { break; }
        d -= diy;
        y += 1;
    }
    let months = if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut mo = 0i64;
    while mo < 12 && d >= months[mo as usize] {
        d -= months[mo as usize];
        mo += 1;
    }

    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", y, mo + 1, d + 1, h, m, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn compute_importance_default() {
        let imp = compute_thinking_importance(&[], false);
        assert!((imp - 0.4).abs() < 0.01);
    }

    #[test]
    fn compute_importance_with_tools() {
        let imp = compute_thinking_importance(&["Glob".into(), "Grep".into()], false);
        assert!((imp - 0.6).abs() < 0.01);
    }

    #[test]
    fn compute_importance_with_correction() {
        let imp = compute_thinking_importance(&["recall".into()], true);
        assert!((imp - 0.9).abs() < 0.01);
    }

    #[test]
    fn compute_importance_capped_at_0_9() {
        let imp = compute_thinking_importance(&["a".into(), "b".into(), "c".into()], true);
        assert!((imp - 0.9).abs() < 0.01);
    }

    #[test]
    fn append_and_read_back() {
        let dir = std::env::temp_dir().join("kat_test_thinking");
        let _ = fs::remove_dir_all(&dir);
        let record = ThinkingRecord {
            turn: 1,
            timestamp: "2026-08-04T16:30:00".into(),
            thinking: "let me think about this...".into(),
            thinking_len: 25,
            importance: 0.6,
            had_correction: false,
            tools_called: vec!["Glob".into()],
            recall_hits: 0,
            previous_thinking_id: None,
        };
        append_thinking(&dir, &record);

        let path = dir.join("session-2026-08-04.jsonl");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("let me think about this"));
        assert!(content.contains("\"turn\":1"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn now_timestamp_is_iso_like() {
        let ts = now_timestamp();
        assert!(ts.starts_with("202"));
        assert!(ts.contains("T"));
        // 格式: YYYY-MM-DDTHH:MM:SS
        assert_eq!(ts.len(), 19);
    }
}
