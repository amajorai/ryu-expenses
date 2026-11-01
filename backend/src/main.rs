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

use std::sync::Arc;

use ryu_expenses::{
    api, mcp, paths, sidecar_link, state::AppState, state::Config, store::ExpenseStore,
};

const MOUNT: &str = "/api/expenses";

fn main() -> Result<()> {
    // Take the sealing key Core delivered out of the environment before any
    // thread starts (the async runtime spawns workers).
    ryu_expenses::seal_keys::capture()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let port = std::env::var("RYU_EXPENSES_PORT")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(ryu_expenses::state::DEFAULT_PORT);
    let data_dir = paths::data_dir();
    let db_path = data_dir.join(ryu_expenses::store::STORE);
    // The sidecar resolves its key normally. The stdio MCP mode shares the
    // sidecar's store and never generates a standalone root: it would otherwise
    // seal under a key the sidecar does not hold.
    let mcp_mode = std::env::args().nth(1).as_deref() == Some("mcp");
    let keys = if mcp_mode {
        ryu_expenses::seal_keys::PluginKeys::resolve_existing(
            ryu_expenses::state::PLUGIN_ID,
            &data_dir,
        )?
    } else {
        ryu_expenses::seal_keys::PluginKeys::resolve(ryu_expenses::state::PLUGIN_ID, &data_dir)?
    };
    let store = ExpenseStore::open_with_keys(db_path.clone(), &keys)
        .with_context(|| format!("opening {}", db_path.display()))?;
    let state = AppState::new(store, Config::from_env(port));

    if mcp_mode {
        return mcp::serve(state, data_dir).await;
    }

    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("binding {address}"))?;
    // Publish the loopback handoff for the stdio `mcp` server, which holds no
    // sealing key (see `sidecar_link`).
    let handoff = sidecar_link::publish(&data_dir, listener.local_addr()?.port())?;
    let gate = Gate {
        state: state.clone(),
        handoff_token: Arc::new(handoff.token.clone()),
    };
    let gated = Router::new()
        .nest(MOUNT, api::routes(state.clone()))
        .layer(from_fn_with_state(gate, bearer_gate));
    let app = Router::new()
        .route("/health", get(health))
        .with_state(state)
        .merge(gated);

    tracing::info!(port, mount = MOUNT, db = %db_path.display(), "ryu-expenses: listening");
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(sidecar_link::shutdown_signal())
        .await;
    sidecar_link::withdraw(&data_dir, &handoff);
    served.context("expense server stopped")?;
    Ok(())
}

#[derive(Clone)]
struct Gate {
    state: AppState,
    handoff_token: Arc<String>,
}

async fn health(State(state): State<AppState>) -> Result<Json<serde_json::Value>, StatusCode> {
    match state.store.counts().await {
        Ok(count) => Ok(Json(json!({
            "ok": true,
            "expenses": count,
            "sealing": state.store.seal_health(),
        }))),
        Err(error) => {
            tracing::error!(error = %error, "expense health check failed");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

async fn bearer_gate(
    State(gate): State<Gate>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let provided = bearer_of(request.headers());
    if sidecar_link::authorize(
        provided.as_deref(),
        gate.state.config.token.as_deref(),
        Some(gate.handoff_token.as_str()),
        request.uri().path(),
        mcp::HANDOFF_PREFIXES,
    ) {
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
