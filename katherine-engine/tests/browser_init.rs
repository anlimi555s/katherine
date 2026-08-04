// browser_init.rs — Browser 验证。最小化以减少 chromiumoxide 连接问题。

use katherine_core::tool::Tool;

#[test]
#[ignore]
fn browser_navigate_and_text() {
    let tool = katherine_engine::tools::browser::BrowserTool::new()
        .expect("BrowserTool::new failed");

    // Navigate
    let r = tool
        .execute(serde_json::json!({"action": "navigate", "url": "https://example.com"}))
        .expect("navigate failed");
    assert!(!r.is_error, "navigate: {}", r.content);

    // Get visible text (doesn't require separate page handle)
    let r = tool
        .execute(serde_json::json!({"action": "get_visible_text"}))
        .expect("get_visible_text failed");
    assert!(!r.is_error, "get_visible_text: {}", r.content);
    assert!(!r.content.is_empty(), "should have visible text");
}
