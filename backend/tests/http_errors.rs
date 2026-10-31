//! HTTP status mapping of the expense routes, through the real router.

use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
    Router,
};
use ryu_expenses::{
    api,
    state::{AppState, Config},
    store::ExpenseStore,
};
use serde_json::{json, Value};
use tower::ServiceExt;

fn app() -> Router {
    let store = ExpenseStore::open_memory().expect("in-memory store");
    api::routes(AppState::new(
        store,
        Config {
            port: 0,
            token: None,
        },
    ))
}

async fn send(app: &Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .expect("response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

fn expense() -> Value {
    json!({
        "spentOn": "2026-08-24",
        "merchant": "Morning Coffee",
        "amountMinor": 650,
        "currency": "usd",
        "category": "Food"
    })
}

#[tokio::test]
async fn a_validation_error_is_422_with_its_message() {
    let app = app();
    let mut bad = expense();
    bad["currency"] = json!("U5D");
    let (status, body) = send(&app, Method::POST, "/expenses", Some(bad)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body,
        json!({ "error": "currency must be three uppercase letters" })
    );

    let (status, _) = send(&app, Method::GET, "/expenses", None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, listed) = send(&app, Method::GET, "/expenses", None).await;
    assert_eq!(listed["expenses"], json!([]));
}

#[tokio::test]
async fn a_bad_list_filter_and_an_empty_patch_are_422() {
    let app = app();
    let (status, body) = send(&app, Method::GET, "/expenses?currency=US", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "currency must be three uppercase letters");

    let (_, created) = send(&app, Method::POST, "/expenses", Some(expense())).await;
    let id = created["id"].as_str().expect("created id").to_owned();
    assert_eq!(created["currency"], "USD");
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/expenses/{id}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "at least one expense field is required");
}

#[tokio::test]
async fn patch_and_delete_of_a_missing_id_are_404() {
    let app = app();
    let (status, body) = send(
        &app,
        Method::PATCH,
        "/expenses/missing",
        Some(json!({ "merchant": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({ "error": "expense not found" }));

    let (status, body) = send(&app, Method::DELETE, "/expenses/missing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({ "error": "expense not found" }));
}

#[tokio::test]
async fn the_summary_route_rejects_an_inverted_range_with_422() {
    let app = app();
    send(&app, Method::POST, "/expenses", Some(expense())).await;
    let (status, body) = send(
        &app,
        Method::GET,
        "/summary?from=2026-09-01&to=2026-08-01",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body, json!({ "error": "from must not be after to" }));

    let (status, body) = send(
        &app,
        Method::GET,
        "/summary?from=2026-08-01&to=2026-09-01&currency=usd",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["summary"]["recordCount"], 1);
    assert_eq!(body["summary"]["totalMinor"], 650);
}
