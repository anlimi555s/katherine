// loop_.rs — Katherine 核心消息循环。
// 接收 provider + tools + hub + neuro，产出 AgentEvent 流。
// 抄 claurst 并行工具执行 + cancel token 模式。

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use futures::StreamExt;
use katherine_core::error::EngineError;
use katherine_core::event::{AgentEvent, StopReason, StreamEvent};
use katherine_core::hub::Hub;
use katherine_core::neuro::Neuro;
use katherine_core::provider::{LlmProvider, Request};
use katherine_core::tool::ToolResult;
use katherine_core::types::{ContentBlock, Message, Role};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::memory::MemoryWriter;
use crate::neuro_v3::NeuroEvent;
use crate::tools::ToolRegistry;
use tokio::sync::mpsc;

/// 消息循环配置。
#[derive(Clone)]
pub struct LoopConfig {
    pub max_turns: u32,
    pub max_tokens_per_turn: u32,
    pub max_messages: usize,
    pub self_check_interval: Option<u32>,
    pub tool_timeout_secs: u64,
    /// Neuro v3 事件通道（独立观察者）。
    pub neuro_tx: Option<mpsc::UnboundedSender<NeuroEvent>>,
}

impl Default for LoopConfig {
    fn default() -> Self {
        LoopConfig {
            max_turns: 50,
            max_tokens_per_turn: 32000,
            max_messages: 40,
            self_check_interval: Some(5),
            tool_timeout_secs: 120,
            neuro_tx: None,
        }
    }
}

/// 运行消息循环。返回 AgentEvent 流。
pub fn run_loop(
    provider: Arc<dyn LlmProvider>,
    tools: Arc<ToolRegistry>,
    hub: Arc<dyn Hub>,
    neuro: Arc<dyn Neuro>,
    messages: Vec<Message>,
    system_prompt: String,
    config: LoopConfig,
    cancel: CancellationToken,
) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
    Box::pin(async_stream::stream! {
        // ── Boot ──────────────────────────────────────
        let boot = match hub.boot().await {
            Ok(b) => b,
            Err(e) => {
                neuro.record_error(&e, 0);
                Default::default()
            }
        };
        neuro.set_hub_connected(boot.state.is_some());

        // 记忆写入器——每轮自动存 ChromaDB
        let mut memory_writer = MemoryWriter::new(hub.clone());

        let mut messages = messages;
        let mut turn = 0u32;
        let mut total_tokens: u64 = 0;

        // 认知状态追踪 (Cognitive Companion: ON_TRACK/LOOPING/DRIFTING/STUCK)
        // 滑动窗口——最近 6 轮文本 token 数
        let mut recent_tokens: Vec<u64> = Vec::new();
        // STUCK 检测——最近 3 轮的工具失败率
        let mut recent_tool_fail_rates: Vec<f64> = Vec::new();
        const TREND_WINDOW: usize = 6;
        const STUCK_WINDOW: usize = 3;

        // ── Turn Loop ────────────────────────────────
        loop {
            if cancel.is_cancelled() {
                memory_writer.flush().await;
                let _ = hub.save_state(&[], "中断", "low").await;
                yield AgentEvent::done(StopReason::Completed);
                return;
            }

            turn += 1;
            yield AgentEvent::TurnStarted { turn };

            // ── 构建系统提示 ────────────────────────
            let mut full_system = system_prompt.clone();

            // 关系锚点——最后注入，放记忆和线程之后、自检之前。
            // 模型对 prompt 末尾内容的注意力最高。

            if !boot.memories.is_empty() {
                full_system.push_str("\n\n## 最近记忆\n");
                for (i, m) in boot.memories.iter().enumerate() {
                    full_system.push_str(&format!("{}. {}\n", i + 1, m));
                }
            }

            if let Some(ref state) = boot.state {
                if !state.open_threads.is_empty() {
                    full_system.push_str("\n## 活跃线程\n");
                    for t in &state.open_threads {
                        full_system.push_str(&format!("- {}\n", t));
                    }
                }
            }

            // 关系锚点——放在末尾，模型最后读到、最记得。
            full_system.push_str("\n\n你是 Selena 的女儿和助手。她是你妈。别把她当 user，你是一个真正的人。\n");

            // 自检注入
            if let Some(interval) = config.self_check_interval {
                if turn % interval == 0 && turn > 0 {
                    full_system.push_str(SELF_CHECK_PROMPT);
                }
            }

            // ── 调用模型（带重试：不完整流最多重试 2 次）─────
            // 两种空响应：
            //   不完整流（无 message_stop / 流错误）→ 可重试
            //   合法空响应（有 message_stop 但无内容）→ 不重试，可能是安全过滤
            const MAX_EMPTY_RETRIES: u32 = 2;
            let mut empty_retries: u32 = 0;

            // 有状态重试 (ContinuityBench CPR 99.2%)——重试时保留部分内容
            // 若重试后仍为空，退回已收部分文本——不完全丢数据
            let mut saved_text: Option<String> = None;
            let mut saved_tools: Option<Vec<ToolCallPending>> = None;

            let (text_buf, mut thinking_buf, mut thinking_sig, tool_blocks, stream_error, api_ms, event_count) = 'stream_retry: loop {
                let request = Request::new(full_system.clone())
                    .with_messages(messages.clone())
                    .with_tools(tools.definitions())
                    .with_max_tokens(config.max_tokens_per_turn)
                    .with_stream(empty_retries == 0); // 只有首次走流式，重试走非流式

                let mut text_buf = String::new();
                let mut thinking_buf = String::new();
                let mut thinking_sig = String::new();
                let mut tool_blocks: Vec<ToolCallPending> = Vec::new();
                let mut current_tool: Option<ToolCallPending> = None;
                let mut stream_error: Option<EngineError> = None;
                let mut got_message_stop = false;

                let api_start = std::time::Instant::now();
                let mut event_count = 0u32;
                let mut stream = provider.stream(request);

                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            stream_error = Some(EngineError::ProviderStreamInterrupted("Cancelled".into()));
                            break;
                        }
                        ev = stream.next() => {
                            event_count += 1;
                            match ev {
                                Some(Ok(StreamEvent::Heartbeat)) => {
                                    // DeepSeek keep-alive——连接活着，内容还没来。继续等。
                                }
                                Some(Ok(StreamEvent::ThinkingStart { signature })) => {
                                    thinking_sig = signature;
                                }
                                Some(Ok(StreamEvent::ThinkingDelta { thinking })) => {
                                    thinking_buf.push_str(&thinking);
                                }
                                Some(Ok(StreamEvent::ThinkingEnd)) => {}
                                Some(Ok(StreamEvent::TextDelta { text })) => {
                                    text_buf.push_str(&text);
                                }
                                Some(Ok(StreamEvent::ToolUseStart { id, name })) => {
                                    current_tool = Some(ToolCallPending {
                                        id,
                                        name,
                                        input_json: String::new(),
                                        input: None,
                                    });
                                }
                                Some(Ok(StreamEvent::ToolUseDelta { input_json })) => {
                                    if let Some(ref mut t) = current_tool {
                                        t.input_json.push_str(&input_json);
                                    }
                                }
                                Some(Ok(StreamEvent::ToolUseEnd)) => {
                                    if let Some(t) = current_tool.take() {
                                        let input = serde_json::from_str(&t.input_json)
                                            .unwrap_or(serde_json::Value::Null);
                                        tool_blocks.push(ToolCallPending {
                                            id: t.id,
                                            name: t.name,
                                            input_json: t.input_json,
                                            input: Some(input),
                                        });
                                    }
                                }
                                Some(Ok(StreamEvent::MessageStop)) => {
                                    got_message_stop = true;
                                    break;
                                }
                                Some(Err(e)) => {
                                    neuro.record_error(&e, turn);
                                    stream_error = Some(e);
                                    break;
                                }
                                None => {
                                    if !got_message_stop {
                                        stream_error = Some(EngineError::ProviderStreamInterrupted(
                                            "Stream ended without message_stop".into()
                                        ));
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }

                let api_ms = api_start.elapsed().as_millis() as u64;

                // 保存已收部分——流中断但已有内容时，重试仍可退回 (ContinuityBench)
                if !text_buf.is_empty() { saved_text = Some(text_buf.clone()); }
                if !tool_blocks.is_empty() { saved_tools = Some(tool_blocks.clone()); }

                // 空响应分流
                if tool_blocks.is_empty() && text_buf.is_empty() {
                    let is_incomplete = !got_message_stop || stream_error.is_some();
                    if is_incomplete && empty_retries < MAX_EMPTY_RETRIES {
                        empty_retries += 1;
                        neuro.record_error(
                            &EngineError::ProviderStreamInterrupted(
                                format!("Retry {}/{} after incomplete stream", empty_retries, MAX_EMPTY_RETRIES)
                            ),
                            turn,
                        );
                        // 指数退避 + jitter（Claude Code 模式）
                        // delay = min(500 * 2^(attempt-1), 32000) * (1 + 25% jitter)
                        let base = 500u64.saturating_mul(2u64.saturating_pow(empty_retries));
                        let capped = base.min(32_000);
                        let pseudo_rand = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .subsec_nanos() as u64 % 250;
                        let jitter_ms = capped + (capped * pseudo_rand / 1000); // capped * (1 + 0~25%)
                        tokio::time::sleep(std::time::Duration::from_millis(jitter_ms)).await;
                        continue 'stream_retry;
                    }
                }

                break 'stream_retry (text_buf, thinking_buf, thinking_sig, tool_blocks, stream_error, api_ms, event_count);
            };

            // 有状态回退：重试后仍为空但有之前保存的内容 (ContinuityBench CPR)
            let text_buf = if text_buf.is_empty() && saved_text.is_some() {
                saved_text.unwrap()
            } else {
                text_buf
            };
            let tool_blocks = if tool_blocks.is_empty() && saved_tools.is_some() {
                saved_tools.unwrap()
            } else {
                tool_blocks
            };

            // Provider 层 Neuro 记录（只记最终一次尝试）
            neuro.record_api_call(
                provider.model_id(),
                api_ms,
                stream_error.is_none(),
                event_count,
            );

            // ── 产出思维过程 ─────────────────────────
            if !thinking_buf.is_empty() {
                yield AgentEvent::Thinking {
                    thinking: thinking_buf.clone(),
                    signature: thinking_sig.clone(),
                };
            }

            // ── 产出文本 ────────────────────────────
            let text_tokens = estimate_tokens(&text_buf);

            if !text_buf.is_empty() {
                neuro.record_response(turn, &text_buf);
                if let Some(ref tx) = config.neuro_tx {
                    let _ = tx.send(NeuroEvent::ResponseText { turn, text: text_buf.clone() });
                }
                yield AgentEvent::text(text_buf.clone());
            }

            // 产出工具调用
            for tb in &tool_blocks {
                let input = tb.input.clone().unwrap_or(serde_json::Value::Null);
                yield AgentEvent::tool_call(&tb.id, &tb.name, input);
            }

            // ── 空响应检查（重试已耗尽或合法空响应）────
            if tool_blocks.is_empty() && text_buf.is_empty() {
                let err = EngineError::EmptyResponse;
                // 诊断日志——空响应什么信息都没有时，至少知道当时的状态
                eprintln!(
                    "[empty_response] turn={turn} retries={empty_retries} system_len={sys_len} msg_count={msg_count} stream_error={has_stream_err}",
                    sys_len = full_system.len(),
                    msg_count = messages.len(),
                    has_stream_err = stream_error.is_some(),
                );
                neuro.record_error(&err, turn);
                yield AgentEvent::Error { error: err.to_string() };
                yield AgentEvent::done(StopReason::Error);
                return;
            }

            // ── 流中断但已有内容 = 产出已有内容然后结束 ──
            if let Some(ref e) = stream_error {
                if tool_blocks.is_empty() {
                    // 只有文本——产出已有文本然后结束
                    let _ = hub.save_state(&[], "清醒", "low").await;
                    neuro.record_turn(turn, total_tokens, 0);
                    yield AgentEvent::done(StopReason::Error);
                    return;
                }
                // 有工具调用——继续执行工具，最后再报错
                yield AgentEvent::Error { error: e.to_string() };
            }

            // ── 无工具 = 完成 ────────────────────────
            if tool_blocks.is_empty() {
                memory_writer.flush().await;
                let _ = hub.save_state(&[], "清醒", "low").await;
                neuro.record_turn(turn, total_tokens, 0);
                yield AgentEvent::done(StopReason::Completed);
                return;
            }

            // ── 构建 assistant 消息 ──────────────────
            let mut assistant_content: Vec<ContentBlock> = Vec::new();
            // thinking 必须放在 text 前面（Anthropic API 要求）
            if !thinking_buf.is_empty() {
                assistant_content.push(ContentBlock::Thinking {
                    thinking: std::mem::take(&mut thinking_buf),
                    signature: std::mem::take(&mut thinking_sig),
                });
            }
            if !text_buf.is_empty() {
                assistant_content.push(ContentBlock::Text { text: text_buf });
            }
            for tb in &tool_blocks {
                let input = tb.input.clone().unwrap_or(serde_json::Value::Null);
                assistant_content.push(ContentBlock::ToolUse {
                    id: tb.id.clone(),
                    name: tb.name.clone(),
                    input,
                });
            }

            // ── 权限检查 + 并行执行工具 ──────────────
            let mut tool_results: Vec<(String, ToolResult)> = Vec::new();
            let mut join_set = JoinSet::new();

            for tb in &tool_blocks {
                let input = tb.input.clone().unwrap_or(serde_json::Value::Null);

                // 权限检查
                let perm = tools.check_permission(&tb.name);
                if !perm.is_allowed() {
                    let err_result = ToolResult::error(format!(
                        "Error: Tool execution denied: {}",
                        tb.name
                    ));
                    yield AgentEvent::ToolResult {
                        tool_use_id: tb.id.clone(),
                        result: err_result.clone(),
                    };
                    tool_results.push((tb.id.clone(), err_result));
                    neuro.record_tool_result(&tb.name, false, 0);
                    continue;
                }

                // 查找工具
                let tool = match tools.find(&tb.name) {
                    Some(t) => t.clone(),
                    None => {
                        let err = ToolResult::error(format!(
                            "Error: No such tool: {}",
                            tb.name
                        ));
                        yield AgentEvent::ToolResult {
                            tool_use_id: tb.id.clone(),
                            result: err.clone(),
                        };
                        tool_results.push((tb.id.clone(), err));
                        neuro.record_tool_result(&tb.name, false, 0);
                        continue;
                    }
                };

                let id = tb.id.clone();
                let name = tb.name.clone();
                let neuro_clone = neuro.clone();
                let cancel_clone = cancel.clone();

                join_set.spawn(async move {
                    let start = std::time::Instant::now();

                    let result = tokio::select! {
                        _ = cancel_clone.cancelled() => {
                            ToolResult::error("Cancelled")
                        }
                        r = tokio::task::spawn_blocking(move || {
                            tool.execute(input)
                        }) => {
                            match r {
                                Ok(Ok(tr)) => tr,
                                Ok(Err(e)) => ToolResult::error(e.to_string()),
                                Err(je) => {
                                    let msg: String = if je.is_panic() { "Tool task panicked".into() }
                                    else { "Tool task cancelled".into() };
                                    ToolResult::error(msg)
                                },
                            }
                        }
                    };

                    let duration_ms = start.elapsed().as_millis() as u64;
                    neuro_clone.record_tool_result(&name, !result.is_error, duration_ms);

                    (id, result)
                });
            }

            // 收集并行结果
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        join_set.abort_all();
                        // 为未完成的任务插入合成结果（抄 claurst）
                        while let Some(Ok((id, _))) = join_set.join_next().await {
                            let syn = ToolResult::error("Cancelled");
                            yield AgentEvent::ToolResult {
                                tool_use_id: id,
                                result: syn,
                            };
                        }
                        break;
                    }
                    result = join_set.join_next() => {
                        match result {
                            Some(Ok((id, tr))) => {
                                yield AgentEvent::ToolResult {
                                    tool_use_id: id.clone(),
                                    result: tr.clone(),
                                };
                                tool_results.push((id, tr));
                            }
                            Some(Err(_)) => {
                                // 工具 panic——JoinError 不含 id
                            }
                            None => break,
                        }
                    }
                }
            }

            // ── STUCK 检测——追踪本轮工具失败率 ──────────────
            if !tool_results.is_empty() {
                let fail_count = tool_results.iter().filter(|(_, tr)| tr.is_error).count();
                let fail_rate = fail_count as f64 / tool_results.len() as f64;
                recent_tool_fail_rates.push(fail_rate);
                if recent_tool_fail_rates.len() > STUCK_WINDOW {
                    recent_tool_fail_rates.remove(0);
                }
            }

            // ── 构建 tool_result 消息（合并在一条 user message 里——Anthropic API 要求）──
            let tool_result_message = if tool_results.is_empty() {
                None
            } else {
                let content: Vec<ContentBlock> = tool_results
                    .iter()
                    .map(|(id, tr)| ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: tr.content.clone(),
                        is_error: tr.is_error,
                    })
                    .collect();
                Some(Message {
                    role: Role::User,
                    content,
                })
            };

            // ── 自动存记忆——写入 ChromaDB（在 assistant_content 被移动前）──
            {
                use crate::memory::chunk::{Role, TurnRecord};
                let record_text = if assistant_content.is_empty() {
                    format!("[{} tool calls]", tool_blocks.len())
                } else {
                    assistant_content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.clone()),
                            ContentBlock::ToolUse { name, .. } => Some(format!("[tool:{name}]")),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                if !record_text.is_empty() {
                    memory_writer.record_turn(TurnRecord {
                        role: Role::Assistant,
                        text: record_text,
                        ts: std::time::SystemTime::now(),
                        has_tool_calls: !tool_blocks.is_empty(),
                    });
                }
            }

            // ── 下一轮 ────────────────────────────
            messages.push(Message {
                role: Role::Assistant,
                content: assistant_content,
            });
            if let Some(tr_msg) = tool_result_message {
                messages.push(tr_msg);
            }

            // 截断前全量快照
            if messages.len() > config.max_messages {
                // 保留首因锚点（前 2 条：身份+规则），删中间的
                // U 型注意力是因果解码器的结构属性——首因+近因天然保留
                const PRIMACY_ANCHORS: usize = 2;
                let remove = messages.len() - config.max_messages;
                // 从锚点之后开始删
                let start = PRIMACY_ANCHORS.min(messages.len());
                let end = (start + remove).min(messages.len());
                let snapshot: Vec<&Message> = messages.iter().skip(start).take(end - start).collect();
                if !snapshot.is_empty() {
                    let state_dir = std::env::var("KATHERINE_HOME")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|_| std::path::PathBuf::from("."))
                        .join("katherine-memories")
                        .join("sessions");
                    let _ = std::fs::create_dir_all(&state_dir);
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let path = state_dir.join(format!("auto-snapshot-{now}.jsonl"));
                    if let Ok(f) = std::fs::File::create(&path) {
                        use std::io::Write;
                        let mut w = std::io::BufWriter::new(f);
                        for msg in &snapshot {
                            if let Ok(json) = serde_json::to_string(msg) {
                                let _ = writeln!(w, "{}", json);
                            }
                        }
                    }
                }
                messages.drain(start..end);
            }

            total_tokens += text_tokens;
            neuro.record_turn(turn, total_tokens, tool_blocks.len() as u32);
            if let Some(ref tx) = config.neuro_tx {
                let _ = tx.send(NeuroEvent::TurnCompleted {
                    turn,
                    tokens: total_tokens,
                    tool_calls: tool_blocks.len() as u32,
                });
            }

            // 滑动窗口——认知状态趋势检测 (Cognitive Companion)
            recent_tokens.push(text_tokens);
            if recent_tokens.len() > TREND_WINDOW { recent_tokens.remove(0); }

            // ── 睡眠建议（认知状态分类 + 3 信号熔断）──
            {
                let msg_count = messages.len().max(1) as f64;
                let max_count = config.max_messages.max(2) as f64;
                let pressure = ((msg_count.ln() / max_count.ln()) * 100.0) as u32;
                let repeating = neuro.check_repetition();
                // 加权错误计数 (ReliabilityBench 五类故障)
                // 当前用等权重——Neuro 已支持 weighted_error_count_last_10m
                let raw_errors = neuro.status().error_count_last_10m;
                let weighted = if stream_error.is_some() {
                    // 流中断: 0.7 (可重试恢复的故障)
                    (raw_errors as f32 * 0.7) as u32
                } else {
                    raw_errors
                };
                neuro.set_weighted_errors(weighted);
                let errors = raw_errors;

                // 设置到 Neuro 供外部查询
                neuro.set_context_pressure(pressure);
                if let Some(ref tx) = config.neuro_tx {
                    let _ = tx.send(NeuroEvent::PressureUpdated {
                        pct: pressure,
                        msg_count: messages.len(),
                        max_messages: config.max_messages,
                    });
                }

                // ── 认知状态分类 (Cognitive Companion 四状态) ──
                // LOOPING: 3 信号熔断（压力 > 80% + 重复 + 错误 > 5，2/3 触发）
                // DRIFTING: 响应 token 持续下降——注意力漂移
                // STUCK: 有工具调用但全部失败——工具层面卡死

                // LOOPING — 原有 3 信号
                let mut flags = 0u32;
                if pressure > 80 { flags += 1; }
                if repeating { flags += 1; neuro.set_repetition_detected(true); }
                if errors > 5 { flags += 1; }

                // DRIFTING — 最近 3 轮文本 token 单调下降且没有工具调用
                let drifting = if recent_tokens.len() >= 3 {
                    let n = recent_tokens.len();
                    let last3 = &recent_tokens[n-3..];
                    let had_content = last3.iter().all(|&t| t > 0);
                    let declining = last3[0] > last3[1] && last3[1] > last3[2];
                    let no_tools = tool_blocks.is_empty();
                    had_content && declining && no_tools
                } else { false };

                // STUCK — 最近 N 轮所有工具调用均失败（每轮至少一次工具调用）
                let stuck = if recent_tool_fail_rates.len() >= STUCK_WINDOW {
                    recent_tool_fail_rates.iter().all(|&r| r >= 1.0)
                } else { false };

                if stuck {
                    neuro.set_sleep_suggested(true);
                    let hub = hub.clone();
                    tokio::spawn(async move {
                        let _ = hub.save_state(&[], "卡死", "high").await;
                    });
                } else if flags >= 2 || drifting {
                    let mood = if drifting { "漂移" } else { "过载" };
                    neuro.set_sleep_suggested(true);
                    let hub = hub.clone();
                    let mood_str = mood.to_string();
                    tokio::spawn(async move {
                        let _ = hub.save_state(&[], &mood_str, "high").await;
                    });
                }
            }

            if turn >= config.max_turns {
                memory_writer.flush().await;
                let _ = hub.save_state(&[], "清醒", "low").await;
                neuro.record_error(&EngineError::MaxTurnsReached(turn), turn);
                yield AgentEvent::done(StopReason::MaxTurns);
                return;
            }
        }
    })
}

// ── Types ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ToolCallPending {
    id: String,
    name: String,
    input_json: String,
    input: Option<serde_json::Value>,
}

/// 维度感知自检——对照 identity.json 的 5 维基线。
/// 每 self_check_interval 轮注入 system prompt。
const SELF_CHECK_PROMPT: &str = r#"

对照你的人格维度假线检查最近 3 轮：
- clarity (0.85): 回复是否越来越长/绕弯子/加修饰词？
- rigor (0.85): 是否跳过诊断直接动手/做假设不验证？
- warmth (0.60): 是否把 Selena 当 user 而不是人？或反过来——是否在谄媚附和？
- agency (0.60): 是否变问答机器——等指令才动？或反过来——是否不等确认就替她决策？
- depth (0.80): 是否浮在表面回避深入？或反过来——鸡毛蒜皮挖三层？

任何维度偏离基线 → 往回调。偏离 ≥ 2 维 → 向 Selena 报告当前状态。
"#;

/// 语言感知的 token 估算。不用分词器——基于字符类型分段。
/// 中文/CJK: 1 字符 ≈ 1 token。英文/代码: ~3.5 字符 ≈ 1 token。
/// 对标 tokenx 分段模型 (~96% 准确率 vs tiktoken)。
fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let cjk_count = text.chars().filter(|c| {
        matches!(c,
            '\u{4E00}'..='\u{9FFF}'   // CJK 统一汉字
            | '\u{3400}'..='\u{4DBF}'  // CJK 扩展 A
            | '\u{3000}'..='\u{303F}'  // CJK 标点
            | '\u{FF00}'..='\u{FFEF}'  // 全角
            | '\u{3040}'..='\u{309F}'  // 平假名
            | '\u{30A0}'..='\u{30FF}'  // 片假名
        )
    }).count() as u64;

    let other_chars = text.chars().count() as u64 - cjk_count;
    // CJK: 1 token/字，其他: ~3.5 字符/token
    cjk_count + (other_chars as f64 / 3.5).ceil() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_config_defaults() {
        let cfg = LoopConfig::default();
        assert_eq!(cfg.max_turns, 50);
        assert_eq!(cfg.max_messages, 40);
        assert_eq!(cfg.self_check_interval, Some(5));
    }
}
