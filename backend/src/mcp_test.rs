//! The keyless MCP process: how it forwards to the sidecar, and that it refuses
//! to touch a ledger it cannot open.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{extract::Request, routing::get, Json, Router};
use base64::Engine as _;
use ryu_crypto::PluginCipher;
use serde_json::json;
use tokio::sync::Mutex;

use super::{call, call_direct, filter_query};
use crate::{
    seal_keys::{KeySource, PluginKeys},
    sidecar_link,
    state::{AppState, Config, PLUGIN_ID},
    store::{ExpenseStore, STORE},
};

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ryu-expenses-mcp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sealing_keys(n: u8) -> PluginKeys {
    let subkey = base64::engine::general_purpose::STANDARD.encode([n; 32]);
    PluginKeys::sealing(
        PluginCipher::from_subkey_b64(PLUGIN_ID, &subkey).unwrap(),
        KeySource::Delivered,
    )
}

fn state_for(store: ExpenseStore) -> AppState {
    AppState::new(
        store,
        Config {
            port: 0,
            token: None,
        },
    )
}

fn memory_state() -> AppState {
    state_for(ExpenseStore::open_memory().unwrap())
}

/// A loopback sidecar that records every request target it receives.
async fn recording_sidecar(dir: &std::path::Path) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorder = seen.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    sidecar_link::publish(dir, listener.local_addr().unwrap().port()).unwrap();
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .fallback(move |request: Request| {
            let recorder = recorder.clone();
            async move {
                let target = request
                    .uri()
                    .path_and_query()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                recorder
                    .lock()
                    .await
                    .push(format!("{} {target}", request.method()));
                Json(json!({ "ok": true }))
            }
        });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    seen
}

#[test]
fn filter_query_percent_encodes_values_and_omits_absent_filters() {
    let query = filter_query(&json!({
        "category": "Food & Drink",
        "from": "2026-08-01",
        "limit": 5
    }))
    .unwrap();
    assert_eq!(
        query,
        "?from=2026-08-01&category=Food%20%26%20Drink&limit=5"
    );
    assert!(!query.contains("to="));
    assert!(!query.contains("currency="));
}

#[test]
fn filter_query_is_empty_without_filters() {
    assert_eq!(filter_query(&json!({})).unwrap(), "");
}

#[test]
fn filter_query_cannot_smuggle_extra_parameters() {
    let query = filter_query(&json!({ "category": "x&limit=500#?/" })).unwrap();
    assert_eq!(query, "?category=x%26limit%3D500%23%3F%2F");
}

#[tokio::test]
async fn update_and_delete_percent_encode_ids_so_the_proxied_path_is_fixed() {
    let dir = temp_dir();
    let seen = recording_sidecar(&dir).await;
    let state = memory_state();

    call(
        &state,
        &dir,
        "update",
        &json!({ "id": "../admin?x=1", "patch": { "amountMinor": 5 } }),
    )
    .await
    .unwrap();
    call(&state, &dir, "delete", &json!({ "id": "a/b?c" }))
        .await
        .unwrap();
    call(&state, &dir, "list", &json!({ "category": "Food & Drink" }))
        .await
        .unwrap();

    let seen = seen.lock().await.clone();
    assert_eq!(
        seen,
        [
            "PATCH /api/expenses/expenses/..%2Fadmin%3Fx%3D1",
            "DELETE /api/expenses/expenses/a%2Fb%3Fc",
            "GET /api/expenses/expenses?category=Food%20%26%20Drink",
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_sealed_ledger_without_a_sidecar_is_refused_not_read_or_written() {
    let dir = temp_dir();
    let db = dir.join(STORE);
    {
        let owner = ExpenseStore::open_with_keys(db.clone(), &sealing_keys(3)).unwrap();
        owner
            .insert(crate::models::NewExpense {
                spent_on: "2026-08-24".to_owned(),
                merchant: "kept".to_owned(),
                amount_minor: 10,
                currency: "USD".to_owned(),
                category: "Food".to_owned(),
                account: None,
                note: None,
            })
            .await
            .unwrap();
    }
    let keyless = ExpenseStore::open_with_keys(db, &PluginKeys::compat()).unwrap();
    assert_eq!(keyless.seal_health()["state"], "unhealthy");
    let state = state_for(keyless);

    for (tool, args) in [
        ("list", json!({})),
        ("summary", json!({})),
        (
            "add",
            json!({
                "spentOn": "2026-08-24",
                "merchant": "new",
                "amountMinor": 1,
                "currency": "USD",
                "category": "Food"
            }),
        ),
    ] {
        let error = call(&state, &dir, tool, &args).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("only the Expense Tracker app holds its key"),
            "{tool}: {error}"
        );
    }
    assert_eq!(state.store.counts().await.unwrap(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn call_direct_rejects_missing_patch_blank_id_and_unknown_tools() {
    let state = memory_state();
    let missing_patch = call_direct(&state, "update", &json!({ "id": "x" }))
        .await
        .unwrap_err();
    assert!(missing_patch.to_string().contains("patch is required"));

    for tool in ["update", "delete"] {
        let blank = call_direct(
            &state,
            tool,
            &json!({ "id": "   ", "patch": { "merchant": "m" } }),
        )
        .await
        .unwrap_err();
        assert!(
            blank.to_string().contains("id is required"),
            "{tool}: {blank}"
        );
    }

    let unknown = call_direct(&state, "drop_table", &json!({}))
        .await
        .unwrap_err();
    assert_eq!(unknown.to_string(), "unknown tool 'drop_table'");
}
