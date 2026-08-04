// browser_init.rs — 验证 BrowserTool lazy init + 基本操作。

use katherine_core::tool::Tool;

#[test]
#[ignore]
fn browser_tool_navigate() {
    let tool = katherine_engine::tools::browser::BrowserTool::new()
        .expect("BrowserTool::new failed");

    let result = tool
        .execute(serde_json::json!({"action": "navigate", "url": "https://example.com"}))
        .expect("navigate failed");

    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("Navigated"));
}
