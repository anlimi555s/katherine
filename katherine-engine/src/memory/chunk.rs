// memory/chunk.rs — 模式感知切块逻辑。
// 只依赖时间戳、角色、工具调用——不分析文本内容。
// 未来加 speaker_id 字段即可支持多人直播模式。

use std::time::{Duration, SystemTime};

/// 一轮 turn 的记录。字段最小——不存完整消息。
#[derive(Debug, Clone)]
pub struct TurnRecord {
    pub role: Role,
    pub text: String,
    pub ts: SystemTime,
    pub has_tool_calls: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    Work,
    Chat,
}

/// 检测会话模式：最近 10 轮有工具调用 → 工作模式。
pub fn detect_mode(buffer: &[TurnRecord]) -> SessionMode {
    let recent = if buffer.len() > 10 { &buffer[buffer.len() - 10..] } else { buffer };
    let tool_turns = recent.iter().filter(|r| r.has_tool_calls).count();
    if tool_turns > 0 {
        SessionMode::Work
    } else {
        SessionMode::Chat
    }
}

/// 尝试从缓冲区提取一个完整块。返回 Some 表示有块要存。
/// 工作模式：工具调用链为单元。找到从 user 消息到工具链结束的跨度。
/// 聊天模式：时间间隔 > 5 分钟或安全上限（2000 字符）。
pub fn try_extract_chunk(buffer: &mut Vec<TurnRecord>, mode: SessionMode) -> Option<String> {
    match mode {
        SessionMode::Work => try_extract_work(buffer),
        SessionMode::Chat => try_extract_chat(buffer),
    }
}

/// 工作模式切块：用户请求 + 助手响应（含工具调用链）为一个工作单元。
fn try_extract_work(buffer: &mut Vec<TurnRecord>) -> Option<String> {
    if buffer.is_empty() {
        return None;
    }

    // 从后往前找：最后一个 user 消息 + 后续所有工具链响应
    let mut last_user_idx: Option<usize> = None;
    for i in (0..buffer.len()).rev() {
        if buffer[i].role == Role::User {
            last_user_idx = Some(i);
            break;
        }
    }

    let split_idx = last_user_idx?;

    // 检查这个 user 之后是否有完整的工具链（有 tool_calls 就至少有一条 assistant）
    let has_tools = buffer[split_idx..].iter().any(|r| r.has_tool_calls);
    let has_chain_end = buffer[split_idx..].iter()
        .filter(|r| r.role == Role::User)
        .count() >= 1; // 当前 user 算在内

    // 如果下一个 user 还没出现，工具链可能还在进行中，等
    if has_tools && !has_chain_end {
        // 有工具但只看到一个 user → 可能工具链还在继续
        // 或者只有一个 user 请求 + assistant 响应（不含工具）
        if split_idx > 0 {
            // 前面还有内容，切出来
            let chunk: Vec<TurnRecord> = buffer.drain(0..split_idx).collect();
            return Some(format_chunk(&chunk));
        }
        return None; // 缓冲区只有一个 user，等
    }

    if split_idx > 0 {
        let chunk: Vec<TurnRecord> = buffer.drain(0..split_idx).collect();
        Some(format_chunk(&chunk))
    } else {
        None // 缓冲区只有当前这个 user，等更多消息
    }
}

/// 聊天模式切块：5 分钟间隔或 2000 字符安全上限。
fn try_extract_chat(buffer: &mut Vec<TurnRecord>) -> Option<String> {
    if buffer.is_empty() {
        return None;
    }

    // 找时间间隔 > 5 分钟的切点
    let mut split_idx: Option<usize> = None;
    for i in 1..buffer.len() {
        if buffer[i].role == Role::User {
            if let Ok(gap) = buffer[i].ts.duration_since(buffer[i - 1].ts) {
                if gap > Duration::from_secs(300) {
                    split_idx = Some(i);
                    break;
                }
            }
        }
    }

    if let Some(idx) = split_idx {
        let chunk: Vec<TurnRecord> = buffer.drain(0..idx).collect();
        return Some(format_chunk(&chunk));
    }

    // 安全上限：2000 字符
    let total_len: usize = buffer.iter().map(|r| r.text.len() + 4).sum(); // +4 for labels
    if total_len > 2000 {
        let mut acc = 0usize;
        for i in 0..buffer.len() {
            acc += buffer[i].text.len() + 4;
            if acc > 1500 && buffer[i].role == Role::User {
                let chunk: Vec<TurnRecord> = buffer.drain(0..i).collect();
                return Some(format_chunk(&chunk));
            }
        }
        // 没有合适的切点，全部输出
        let chunk: Vec<TurnRecord> = buffer.drain(0..).collect::<Vec<_>>();
        return Some(format_chunk(&chunk));
    }

    None
}

/// 缓冲区全部转文本（强制 flush 时用）。
pub fn buffer_to_text(buffer: &[TurnRecord]) -> String {
    format_chunk(buffer)
}

fn format_chunk(records: &[TurnRecord]) -> String {
    let mut lines = Vec::new();
    for r in records {
        let label = match r.role {
            Role::User => "Selena",
            Role::Assistant => "Katherine",
        };
        lines.push(format!("[{label}] {}", r.text));
    }
    lines.join("\n")
}

/// 基于回合结构打分，不分析文本内容。
/// 规则：包含工具调用 → 0.6（操作记录），user 连续纠正 → 0.9（高价值），其他 → 0.5（默认）。
pub fn score_importance(_text: &str, has_tool_calls: bool, is_correction_chain: bool) -> f32 {
    if is_correction_chain {
        0.9
    } else if has_tool_calls {
        0.6
    } else {
        0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> SystemTime { SystemTime::now() }
    fn secs_ago(s: u64) -> SystemTime { now() - Duration::from_secs(s) }

    fn user(text: &str, ts: SystemTime) -> TurnRecord {
        TurnRecord { role: Role::User, text: text.into(), ts, has_tool_calls: false }
    }
    fn assistant(text: &str, ts: SystemTime) -> TurnRecord {
        TurnRecord { role: Role::Assistant, text: text.into(), ts, has_tool_calls: false }
    }
    fn tool_turn(text: &str, ts: SystemTime) -> TurnRecord {
        TurnRecord { role: Role::Assistant, text: text.into(), ts, has_tool_calls: true }
    }

    #[test]
    fn detect_work_mode_with_tools() {
        let mut buf = vec![
            user("hello", secs_ago(100)),
            assistant("hi", secs_ago(99)),
        ];
        // 纯聊天 → chat
        assert_eq!(detect_mode(&buf), SessionMode::Chat);

        buf.push(tool_turn("用 Bash 跑 cargo test", secs_ago(98)));
        // 有工具 → work
        assert_eq!(detect_mode(&buf), SessionMode::Work);
    }

    #[test]
    fn chat_mode_time_gap_splits() {
        let mut buf = vec![
            user("聊架构", secs_ago(1000)),
            assistant("好", secs_ago(999)),
            user("等下", secs_ago(10)),    // 10秒前 → 和上面间隔大
            assistant("好了", secs_ago(9)),
        ];
        let chunk = try_extract_chat(&mut buf);
        assert!(chunk.is_some()); // 前面的被切出来了
        assert_eq!(buf.len(), 2); // 剩下后面的
    }

    #[test]
    fn work_mode_extracts_before_new_user() {
        let mut buf = vec![
            user("修bug", secs_ago(500)),
            tool_turn("Bash: cargo build", secs_ago(499)),
            assistant("修好了", secs_ago(498)),
            user("下一个任务", secs_ago(10)),
        ];
        // 在 "下一个任务" 之前切
        let chunk = try_extract_work(&mut buf);
        assert!(chunk.is_some());
        assert_eq!(buf.len(), 1); // 剩下 "下一个任务"
    }

    #[test]
    fn score_correction_highest() {
        assert!((score_importance("", false, true) - 0.9).abs() < 0.01);
    }

    #[test]
    fn score_work_default() {
        assert!((score_importance("", true, false) - 0.6).abs() < 0.01);
    }

    #[test]
    fn score_normal_low() {
        assert!((score_importance("", false, false) - 0.5).abs() < 0.01);
    }
}
