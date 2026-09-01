use serde_json::json;

use ryu_expenses::{api::SERVED_ROUTES, mcp, state::bearer_ok};

#[test]
fn served_routes_are_the_small_expense_surface() {
    assert_eq!(SERVED_ROUTES, ["/expenses", "/expenses/:id", "/summary"]);
}

#[test]
fn mcp_tools_are_bare_and_descriptive() {
    let table = mcp::tools();
    let tools = table.as_array().expect("MCP tool list");
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect::<Vec<_>>();
    assert_eq!(names, ["list", "summary", "add", "update", "delete"]);
    for tool in tools {
        assert!(tool["description"].as_str().unwrap_or_default().len() > 40);
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(!tool["name"].as_str().unwrap().contains('.'));
    }
}

#[test]
fn bearer_auth_fails_closed() {
    assert!(!bearer_ok(None, Some("secret")));
    assert!(!bearer_ok(Some("wrong"), Some("secret")));
    assert!(!bearer_ok(Some("secret"), None));
    assert!(bearer_ok(Some("secret"), Some("secret")));
}

#[test]
fn mcp_tool_failures_are_result_content() {
    let response = mcp::tool_content(&json!({ "error": "expense not found" }), true);
    assert_eq!(response["isError"], true);
    assert!(response["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("expense not found"));
}
