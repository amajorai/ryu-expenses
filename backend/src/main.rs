use std::net::{Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::{from_fn_with_state, Next},
    response::Response,
    routing::get,
    Json, Router,
};
use serde_json::json;
use tracing_subscriber::EnvFilter;

use ryu_expenses::{api, mcp, paths, state::AppState, state::Config, store::ExpenseStore};

const MOUNT: &str = "/api/expenses";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let port = std::env::var("RYU_EXPENSES_PORT")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(ryu_expenses::state::DEFAULT_PORT);
    let db_path = paths::ryu_dir().join("expenses.db");
    let store = ExpenseStore::open(db_path.clone())
        .with_context(|| format!("opening {}", db_path.display()))?;
    let state = AppState::new(store, Config::from_env(port));

    if std::env::args().nth(1).as_deref() == Some("mcp") {
        return mcp::serve(state).await;
    }

    let gated = Router::new()
        .nest(MOUNT, api::routes(state.clone()))
        .layer(from_fn_with_state(state.clone(), bearer_gate));
    let app = Router::new()
        .route("/health", get(health))
        .with_state(state)
        .merge(gated);

    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("binding {address}"))?;
    tracing::info!(port, mount = MOUNT, db = %db_path.display(), "ryu-expenses: listening");
    axum::serve(listener, app)
        .await
        .context("expense server stopped")?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> Result<Json<serde_json::Value>, StatusCode> {
    match state.store.counts().await {
        Ok(count) => Ok(Json(json!({ "ok": true, "expenses": count }))),
        Err(error) => {
            tracing::error!(error = %error, "expense health check failed");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

async fn bearer_gate(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let provided = bearer_of(request.headers());
    if ryu_expenses::state::bearer_ok(provided.as_deref(), state.config.token.as_deref()) {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn bearer_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
}
