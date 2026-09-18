use std::io::Write as _;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt as _, BufReader};

use crate::{
    models::{ExpenseFilters, ExpensePatch, NewExpense},
    state::AppState,
};

const PROTOCOL_VERSION: &str = "2024-11-05";

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

pub async fn serve(state: AppState) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
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
        let response = match method {
            "initialize" => ok(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "expenses", "version": env!("CARGO_PKG_VERSION") }
                }),
            ),
            "ping" => ok(id, json!({})),
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
                match call(&state, name, &args).await {
                    Ok(value) => ok(id, tool_content(&value, false)),
                    Err(error) => ok(
                        id,
                        tool_content(&json!({ "error": error.to_string() }), true),
                    ),
                }
            }
            other => error(id, -32601, &format!("unknown method '{other}'")),
        };
        emit(&response);
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

async fn call(state: &AppState, name: &str, args: &Value) -> Result<Value> {
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
