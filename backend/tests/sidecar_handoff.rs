//! The stdio MCP server's handoff to the running sidecar: the published token
//! reaches the ledger routes over loopback and nothing else.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{from_fn_with_state, Next},
    response::Response,
    routing::get,
    Router,
};
use base64::Engine as _;
use ryu_crypto::PluginCipher;
use ryu_expenses::{
    api, mcp,
    seal_keys::{KeySource, PluginKeys},
    sidecar_link::{self, SidecarClient},
    state::{AppState, Config, PLUGIN_ID},
    store::{ExpenseStore, STORE},
};
use serde_json::json;

async fn gate(
    State(token): State<Arc<String>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let provided = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned);
    if sidecar_link::authorize(
        provided.as_deref(),
        Some("core-ext-token"),
        Some(token.as_str()),
        request.uri().path(),
        mcp::HANDOFF_PREFIXES,
    ) {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[tokio::test]
async fn the_handoff_reaches_the_sealed_ledger_through_the_sidecar() {
    let dir = std::env::temp_dir().join(format!("ryu-expenses-handoff-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let subkey = base64::engine::general_purpose::STANDARD.encode([11u8; 32]);
    let keys = PluginKeys::sealing(
        PluginCipher::from_subkey_b64(PLUGIN_ID, &subkey).unwrap(),
        KeySource::Delivered,
    );
    let db = dir.join(STORE);
    let store = ExpenseStore::open_with_keys(db.clone(), &keys).unwrap();
    let state = AppState::new(
        store,
        Config {
            port: 0,
            token: Some("core-ext-token".into()),
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handoff = sidecar_link::publish(&dir, listener.local_addr().unwrap().port()).unwrap();
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(
            Router::new()
                .nest("/api/expenses", api::routes(state.clone()))
                .route("/api/expenses/admin", get(|| async { "admin" }))
                .layer(from_fn_with_state(Arc::new(handoff.token.clone()), gate)),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = SidecarClient::connect(&dir).await.expect("sidecar answers");
    let created = client
        .call(
            "POST",
            "/api/expenses/expenses",
            Some(&json!({
                "spentOn": "2026-08-24",
                "merchant": "handoff-merchant-marker",
                "amountMinor": 1234,
                "currency": "USD",
                "category": "Food",
                "note": "kept"
            })),
        )
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_owned();
    let listed = client
        .call("GET", "/api/expenses/expenses?currency=USD", None)
        .await
        .unwrap();
    assert_eq!(listed["expenses"][0]["merchant"], "handoff-merchant-marker");
    // A patch without `note` keeps it.
    let patched = client
        .call(
            "PATCH",
            &format!("/api/expenses/expenses/{id}"),
            Some(&json!({ "amountMinor": 99 })),
        )
        .await
        .unwrap();
    assert_eq!(patched["amountMinor"], 99);
    assert_eq!(patched["note"], "kept");
    let summary = client
        .call("GET", "/api/expenses/summary", None)
        .await
        .unwrap();
    assert_eq!(summary["summary"]["totalMinor"], 99);
    // The handoff token opens nothing outside the ledger routes.
    let denied = client
        .request("GET", "/api/expenses/admin", None)
        .await
        .unwrap();
    assert_eq!(denied.status, 401);
    client
        .call("DELETE", &format!("/api/expenses/expenses/{id}"), None)
        .await
        .unwrap();
    // Sealed on disk.
    let mut bytes = std::fs::read(&db).unwrap();
    bytes.extend(std::fs::read(dir.join(format!("{STORE}-wal"))).unwrap_or_default());
    assert!(!bytes.windows(23).any(|w| w == b"handoff-merchant-marker"));
    sidecar_link::withdraw(&dir, &handoff);
    assert!(SidecarClient::connect(&dir).await.is_none());
}
