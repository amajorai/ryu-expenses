use std::io::Write as _;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt as _, BufReader};

use crate::{
    models::{ExpenseFilters, ExpensePatch, NewExpense},
    state::AppState,
};
use ryu_sidecar_runtime::mcp_protocol::{
    discover_result, finalize_response, protocol_error, request_is_modern, PROTOCOL_VERSION,
};

pub fn tools() -> Value {
    json!([
        {
            "name": "list",
            "description": "List recorded expenses, newest first. Use this before claiming a record exists or answering a date/category spend question. Filters are optional and results are bounded. This is read-only and never changes the ledger.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "Inclusive YYYY-MM-DD start date." },
                    "to": { "type": "string", "description": "Inclusive YYYY-MM-DD end date." },
                    "category": { "type": "string" },
                    "currency": { "type": "string", "description": "Three-letter currency code." },
                    "limit": { "type": "integer", "description": "Maximum rows, up to 500." }
                }
            }
        },
        {
            "name": "summary",
            "description": "Summarize recorded expenses for a date/category filter. Returns record count, totals grouped by currency, and category totals. It never converts currencies or gives financial advice; when currencies differ, do not add their totals.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" },
                    "category": { "type": "string" },
                    "currency": { "type": "string" }
                }
            }
        },
        {
            "name": "add",
            "description": "Record one expense after the user has supplied or confirmed the date, merchant, positive amount in integer minor units, currency, and category. Preserve the user's wording, never invent a missing value, and report the returned record after the write.",
            "inputSchema": {
                "type": "object",
                "required": ["spentOn", "merchant", "amountMinor", "currency", "category"],
                "properties": {
                    "spentOn": { "type": "string", "description": "Exact YYYY-MM-DD date." },
                    "merchant": { "type": "string" },
                    "amountMinor": { "type": "integer", "description": "Positive integer minor units; do not send a floating-point amount." },
                    "currency": { "type": "string" },
                    "category": { "type": "string" },
                    "account": { "type": "string" },
                    "note": { "type": "string" }
                }
            }
        },
        {
            "name": "update",
            "description": "Correct fields on one recorded expense by id. Send only the fields that changed, then use the returned record as the source of truth. Do not silently reinterpret a currency or date.",
            "inputSchema": {
                "type": "object",
                "required": ["id", "patch"],
                "properties": {
                    "id": { "type": "string" },
                    "patch": { "type": "object", "description": "Partial expense fields using the add shape." }
                }
            }
        },
        {
            "name": "delete",
            "description": "Permanently delete one recorded expense by id. Only call this after the user explicitly asks to remove that record; report the sidecar result and never claim deletion if it failed.",
            "inputSchema": {
                "type": "object",
                "required": ["id"],
                "properties": { "id": { "type": "string" } }
            }
        }
    ])
}

/// Serve MCP on stdio. `data_dir` is the plugin data dir: when the sidecar
/// published a handoff there, every tool goes through it (it holds the
/// sealing key); otherwise the store opened by this process is used.
pub async fn serve(state: AppState, data_dir: std::path::PathBuf) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut legacy_initialized = false;
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(line) else {
            tracing::warn!("mcp: skipping invalid JSON frame");
            continue;
        };
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        let modern = match request_is_modern(&request, method, legacy_initialized) {
            Ok(modern) => modern,
            Err(error) => {
                emit(&protocol_error(id, error));
                continue;
            }
        };
        legacy_initialized |= method == "initialize" && !modern;
        let response = match method {
            "initialize" if modern => error(id, -32601, "unknown method 'initialize'"),
            "server/discover" => ok(id, discover_result()),
            "initialize" => ok(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "expenses", "version": env!("CARGO_PKG_VERSION") }
                }),
            ),
            "ping" if !modern => ok(id, json!({})),
            "ping" => error(id, -32601, "unknown method 'ping'"),
            "tools/list" => ok(id, json!({ "tools": tools() })),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match call(&state, &data_dir, name, &args).await {
                    Ok(value) => ok(id, tool_content(&value, false)),
                    Err(error) => ok(
                        id,
                        tool_content(&json!({ "error": error.to_string() }), true),
                    ),
                }
            }
            other => error(id, -32601, &format!("unknown method '{other}'")),
        };
        emit(&finalize_response(
            response,
            modern,
            method,
            "expenses",
            env!("CARGO_PKG_VERSION"),
        ));
    }
    Ok(())
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub fn tool_content(value: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn emit(frame: &Value) {
    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{frame}").is_ok() {
        let _ = stdout.flush();
    }
}

/// The sidecar route prefixes the handoff token may call.
pub const HANDOFF_PREFIXES: &[&str] = &["/api/expenses/expenses", "/api/expenses/summary"];

async fn call(
    state: &AppState,
    data_dir: &std::path::Path,
    name: &str,
    args: &Value,
) -> Result<Value> {
    if let Some(client) = crate::sidecar_link::SidecarClient::connect(data_dir).await {
        return call_sidecar(&client, name, args).await;
    }
    if state.store.seal_health()["state"] == "unhealthy" {
        anyhow::bail!(
            "the expense ledger is encrypted and only the Expense Tracker app holds its key; \
             open the app in Ryu and try again"
        );
    }
    call_direct(state, name, args).await
}

fn required_id(args: &Value) -> Result<&str> {
    args.get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .context("id is required")
}

fn filter_query(args: &Value) -> Result<String> {
    let filters = filters(args)?;
    let mut pairs = Vec::new();
    for (key, value) in [
        ("from", filters.from),
        ("to", filters.to),
        ("category", filters.category),
        ("currency", filters.currency),
        ("limit", filters.limit.map(|limit| limit.to_string())),
    ] {
        if let Some(value) = value {
            pairs.push(format!("{key}={}", percent_encode(&value)));
        }
    }
    Ok(if pairs.is_empty() {
        String::new()
    } else {
        format!("?{}", pairs.join("&"))
    })
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The same tools through the running sidecar's HTTP API.
async fn call_sidecar(
    client: &crate::sidecar_link::SidecarClient,
    name: &str,
    args: &Value,
) -> Result<Value> {
    match name {
        "list" => {
            let path = format!("/api/expenses/expenses{}", filter_query(args)?);
            client.call("GET", &path, None).await
        }
        "summary" => {
            let path = format!("/api/expenses/summary{}", filter_query(args)?);
            client.call("GET", &path, None).await
        }
        "add" => {
            let input: NewExpense =
                serde_json::from_value(args.clone()).context("invalid add input")?;
            client
                .call(
                    "POST",
                    "/api/expenses/expenses",
                    Some(&serde_json::to_value(input)?),
                )
                .await
        }
        "update" => {
            let id = required_id(args)?;
            // Forward the patch as given: an omitted field keeps its value and
            // an explicit null clears it, which re-serializing would lose.
            let patch = args.get("patch").cloned().context("patch is required")?;
            let _: ExpensePatch =
                serde_json::from_value(patch.clone()).context("invalid update input")?;
            let path = format!("/api/expenses/expenses/{}", percent_encode(id));
            client.call("PATCH", &path, Some(&patch)).await
        }
        "delete" => {
            let id = required_id(args)?;
            let path = format!("/api/expenses/expenses/{}", percent_encode(id));
            client.call("DELETE", &path, None).await
        }
        other => Err(anyhow::anyhow!("unknown tool '{other}'")),
    }
}

async fn call_direct(state: &AppState, name: &str, args: &Value) -> Result<Value> {
    match name {
        "list" => Ok(json!({
            "expenses": state.store.list(filters(args)?).await?
        })),
        "summary" => Ok(json!({
            "summary": state.store.summary(filters(args)?).await?
        })),
        "add" => {
            let input: NewExpense =
                serde_json::from_value(args.clone()).context("invalid add input")?;
            Ok(serde_json::to_value(state.store.insert(input).await?)?)
        }
        "update" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .context("id is required")?;
            let patch: ExpensePatch =
                serde_json::from_value(args.get("patch").cloned().context("patch is required")?)
                    .context("invalid update input")?;
            Ok(serde_json::to_value(state.store.update(id, patch).await?)?)
        }
        "delete" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .context("id is required")?;
            state.store.delete(id).await?;
            Ok(json!({ "ok": true }))
        }
        other => Err(anyhow::anyhow!("unknown tool '{other}'")),
    }
}

fn filters(args: &Value) -> Result<ExpenseFilters> {
    serde_json::from_value(args.clone()).context("invalid expense filters")
}

#[cfg(test)]
#[path = "mcp_test.rs"]
mod tests;
