// mock_provider.rs — 确定性 Mock Provider。
// 预设 StreamEvent 序列，不碰网络。
// 抄 claw-code mock-anthropic-service 范式。

use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use futures::Stream;
use katherine_core::error::EngineError;
use katherine_core::event::StreamEvent;
use katherine_core::provider::{LlmProvider, Request};

// ── noop waker ────────────────────────────────────────────

/// 不需要唤醒的 waker——MockStream 是同步的。
fn noop_waker() -> Waker {
    unsafe fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null::<()>(), &VTABLE)
    }
    unsafe fn noop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null::<()>(), &VTABLE)) }
}

// ── MockStream ────────────────────────────────────────────

/// 从预定义事件列表生成流。无异步——读 Vec 而已。
struct MockStream {
    events: Vec<Result<StreamEvent, EngineError>>,
    index: usize,
}

impl Stream for MockStream {
    type Item = Result<StreamEvent, EngineError>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.index >= self.events.len() {
            return Poll::Ready(None);
        }
        let item = self.events[self.index].clone();
        self.index += 1;
        Poll::Ready(Some(item))
    }
}

// ── MockProvider ──────────────────────────────────────────

/// 确定性 mock。每轮调用消耗 events 列表中的一个元素（一轮 = 一个 Vec<StreamEvent>）。
/// 一轮结束后自动切到下一轮的事件序列。
pub struct MockProvider {
    model_name: String,
    turn_events: Mutex<Vec<Vec<Result<StreamEvent, EngineError>>>>,
    turn_index: Mutex<usize>,
}

impl MockProvider {
    pub fn new(model_name: impl Into<String>) -> Self {
        MockProvider {
            model_name: model_name.into(),
            turn_events: Mutex::new(Vec::new()),
            turn_index: Mutex::new(0),
        }
    }

    // ── 场景工厂 ──────────────────────────────────────

    /// 纯文本回复——一轮结束。
    pub fn text_only(text: impl Into<String>) -> Self {
        let p = MockProvider::new("mock:text");
        p.add_turn(vec![
            Ok(StreamEvent::TextDelta {
                text: text.into(),
            }),
            Ok(StreamEvent::MessageStop),
        ]);
        p
    }

    /// 单工具调用——含完整 tool_use 序列。
    pub fn single_tool(id: &str, name: &str, input: serde_json::Value) -> Self {
        let input_json = input.to_string();
        let p = MockProvider::new("mock:tool");
        p.add_turn(vec![
            Ok(StreamEvent::ToolUseStart {
                id: id.to_string(),
                name: name.to_string(),
            }),
            Ok(StreamEvent::ToolUseDelta {
                input_json,
            }),
            Ok(StreamEvent::ToolUseEnd),
            Ok(StreamEvent::MessageStop),
        ]);
        p
    }

    /// 多工具并行调用。
    pub fn multi_tool(calls: Vec<(&str, &str, serde_json::Value)>) -> Self {
        let p = MockProvider::new("mock:multi-tool");
        let mut events: Vec<Result<StreamEvent, EngineError>> = Vec::new();
        for (id, name, input) in &calls {
            events.push(Ok(StreamEvent::ToolUseStart {
                id: id.to_string(),
                name: name.to_string(),
            }));
            events.push(Ok(StreamEvent::ToolUseDelta {
                input_json: input.to_string(),
            }));
            events.push(Ok(StreamEvent::ToolUseEnd));
        }
        events.push(Ok(StreamEvent::MessageStop));
        p.add_turn(events);
        p
    }

    /// 流中断——有文本但无 MessageStop。
    pub fn stream_interrupted(partial_text: &str) -> Self {
        let p = MockProvider::new("mock:interrupted");
        p.add_turn(vec![Ok(StreamEvent::TextDelta {
            text: partial_text.to_string(),
        })]);
        p
    }

    /// max_tokens → 空响应（模拟截断后无更多内容）。
    pub fn max_tokens_then_empty() -> Self {
        let p = MockProvider::new("mock:max-tokens");
        p.add_turn(vec![
            Ok(StreamEvent::TextDelta {
                text: "第一轮截断输出...".into(),
            }),
            Ok(StreamEvent::MessageStop),
        ]);
        p.add_turn(vec![Ok(StreamEvent::MessageStop)]);
        p
    }

    /// 多轮对话场景。
    pub fn multi_turn(turns: Vec<Vec<Result<StreamEvent, EngineError>>>) -> Self {
        let p = MockProvider::new("mock:multi-turn");
        for turn in turns {
            p.add_turn(turn);
        }
        p
    }

    // ── 构造辅助 ──────────────────────────────────────

    fn add_turn(&self, events: Vec<Result<StreamEvent, EngineError>>) -> &Self {
        self.turn_events.lock().unwrap().push(events);
        self
    }
}

impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        &self.model_name
    }

    fn stream(
        &self,
        _request: Request,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, EngineError>> + Send + '_>> {
        let turns = self.turn_events.lock().unwrap();
        let mut index = self.turn_index.lock().unwrap();

        let events = if *index < turns.len() {
            let ev = turns[*index].clone();
            *index += 1;
            ev
        } else {
            vec![]
        };

        Box::pin(MockStream { events, index: 0 })
    }
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_text_only_events() {
        let provider = MockProvider::text_only("你好");
        let req = Request::new("test system");

        let mut stream = provider.stream(req);
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let event1 = Pin::new(&mut stream).poll_next(&mut cx);
        assert!(matches!(
            event1,
            Poll::Ready(Some(Ok(StreamEvent::TextDelta { .. })))
        ));

        let event2 = Pin::new(&mut stream).poll_next(&mut cx);
        assert!(matches!(
            event2,
            Poll::Ready(Some(Ok(StreamEvent::MessageStop)))
        ));

        let event3 = Pin::new(&mut stream).poll_next(&mut cx);
        assert!(matches!(event3, Poll::Ready(None)));
    }

    #[test]
    fn mock_single_tool_events() {
        let provider =
            MockProvider::single_tool("t1", "Read", serde_json::json!({"file_path": "/tmp/x"}));
        let req = Request::new("test");

        let mut stream = provider.stream(req);
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        match Pin::new(&mut stream).poll_next(&mut cx) {
            Poll::Ready(Some(Ok(StreamEvent::ToolUseStart { id, name }))) => {
                assert_eq!(id, "t1");
                assert_eq!(name, "Read");
            }
            other => panic!("expected ToolUseStart, got {other:?}"),
        }

        assert!(matches!(
            Pin::new(&mut stream).poll_next(&mut cx),
            Poll::Ready(Some(Ok(StreamEvent::ToolUseDelta { .. })))
        ));
        assert!(matches!(
            Pin::new(&mut stream).poll_next(&mut cx),
            Poll::Ready(Some(Ok(StreamEvent::ToolUseEnd)))
        ));
        assert!(matches!(
            Pin::new(&mut stream).poll_next(&mut cx),
            Poll::Ready(Some(Ok(StreamEvent::MessageStop)))
        ));
        assert!(matches!(
            Pin::new(&mut stream).poll_next(&mut cx),
            Poll::Ready(None)
        ));
    }

    #[test]
    fn mock_multi_turn_rounds() {
        let provider = MockProvider::multi_turn(vec![
            vec![
                Ok(StreamEvent::TextDelta {
                    text: "turn 1".into(),
                }),
                Ok(StreamEvent::MessageStop),
            ],
            vec![
                Ok(StreamEvent::TextDelta {
                    text: "turn 2".into(),
                }),
                Ok(StreamEvent::MessageStop),
            ],
        ]);

        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let mut s1 = provider.stream(Request::new("sys"));
        let t1 = match Pin::new(&mut s1).poll_next(&mut cx) {
            Poll::Ready(Some(Ok(StreamEvent::TextDelta { text }))) => text,
            other => panic!("expected TextDelta, got {other:?}"),
        };
        assert_eq!(t1, "turn 1");

        let mut s2 = provider.stream(Request::new("sys"));
        let t2 = match Pin::new(&mut s2).poll_next(&mut cx) {
            Poll::Ready(Some(Ok(StreamEvent::TextDelta { text }))) => text,
            other => panic!("expected TextDelta, got {other:?}"),
        };
        assert_eq!(t2, "turn 2");
    }

    #[test]
    fn mock_stream_interrupted_no_message_stop() {
        let provider = MockProvider::stream_interrupted("前半段");
        let mut stream = provider.stream(Request::new("sys"));

        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let ev = Pin::new(&mut stream).poll_next(&mut cx);
        assert!(matches!(
            ev,
            Poll::Ready(Some(Ok(StreamEvent::TextDelta { .. })))
        ));

        let ev = Pin::new(&mut stream).poll_next(&mut cx);
        assert!(matches!(ev, Poll::Ready(None)));
    }

    #[test]
    fn mock_max_tokens_then_empty() {
        let provider = MockProvider::max_tokens_then_empty();
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        // Turn 1: 有内容
        let mut s1 = provider.stream(Request::new("sys"));
        assert!(matches!(
            Pin::new(&mut s1).poll_next(&mut cx),
            Poll::Ready(Some(Ok(StreamEvent::TextDelta { .. })))
        ));

        // Turn 2: 空
        let mut s2 = provider.stream(Request::new("sys"));
        assert!(matches!(
            Pin::new(&mut s2).poll_next(&mut cx),
            Poll::Ready(Some(Ok(StreamEvent::MessageStop)))
        ));
        assert!(matches!(
            Pin::new(&mut s2).poll_next(&mut cx),
            Poll::Ready(None)
        ));
    }
}
