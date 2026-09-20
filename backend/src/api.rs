use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    models::{ExpenseFilters, ExpensePatch, NewExpense},
    state::AppState,
};

pub const SERVED_ROUTES: &[&str] = &["/expenses", "/expenses/:id", "/summary"];

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn from_error(error: anyhow::Error) -> Self {
        let message = error.to_string();
        let status = if error
            .downcast_ref::<crate::validation::ValidationError>()
            .is_some()
        {
            StatusCode::UNPROCESSABLE_ENTITY
        } else if message == "expense not found" {
            StatusCode::NOT_FOUND
        } else {
            tracing::error!(error = %message, "expense tracker request failed");
            StatusCode::INTERNAL_SERVER_ERROR
        };
        Self { status, message }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExpenseQuery {
    from: Option<String>,
    to: Option<String>,
    category: Option<String>,
    currency: Option<String>,
    limit: Option<i64>,
}

impl From<ExpenseQuery> for ExpenseFilters {
    fn from(query: ExpenseQuery) -> Self {
        Self {
            from: query.from,
            to: query.to,
            category: query.category,
            currency: query.currency,
            limit: query.limit,
        }
    }
}

pub fn routes(state: AppState) -> Router<()> {
    Router::new()
        .route("/expenses", get(list_expenses).post(create_expense))
        .route(
            "/expenses/:id",
            patch(update_expense).delete(delete_expense),
        )
        .route("/summary", get(get_summary))
        .with_state(state)
}

async fn list_expenses(
    State(state): State<AppState>,
    Query(query): Query<ExpenseQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let expenses = state
        .store
        .list(query.into())
        .await
        .map_err(ApiError::from_error)?;
    Ok(Json(json!({ "expenses": expenses })))
}

async fn create_expense(
    State(state): State<AppState>,
    Json(input): Json<NewExpense>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let expense = state
        .store
        .insert(input)
        .await
        .map_err(ApiError::from_error)?;
    Ok(Json(
        serde_json::to_value(expense).expect("expense serializes"),
    ))
}

async fn update_expense(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(patch): Json<ExpensePatch>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let expense = state
        .store
        .update(&id, patch)
        .await
        .map_err(ApiError::from_error)?;
    Ok(Json(
        serde_json::to_value(expense).expect("expense serializes"),
    ))
}

async fn delete_expense(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .store
        .delete(&id)
        .await
        .map_err(ApiError::from_error)?;
    Ok(Json(json!({ "ok": true })))
}

async fn get_summary(
    State(state): State<AppState>,
    Query(query): Query<ExpenseQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let summary = state
        .store
        .summary(query.into())
        .await
        .map_err(ApiError::from_error)?;
    Ok(Json(json!({ "summary": summary })))
}
