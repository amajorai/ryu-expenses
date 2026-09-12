use serde::{Deserialize, Serialize};

/// An expense as stored by the sidecar. `amount_minor` is always an integer;
/// the currency decides how a client formats it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Expense {
    pub id: String,
    pub spent_on: String,
    pub merchant: String,
    pub amount_minor: i64,
    pub currency: String,
    pub category: String,
    pub account: Option<String>,
    pub note: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewExpense {
    pub spent_on: String,
    pub merchant: String,
    pub amount_minor: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub category: String,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExpensePatch {
    pub spent_on: Option<String>,
    pub merchant: Option<String>,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub category: Option<String>,
    pub account: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseFilters {
    pub from: Option<String>,
    pub to: Option<String>,
    pub category: Option<String>,
    pub currency: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CategoryTotal {
    pub category: String,
    pub total_minor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CurrencySummary {
    pub currency: String,
    pub total_minor: i64,
    pub categories: Vec<CategoryTotal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseSummary {
    pub record_count: usize,
    pub total_minor: Option<i64>,
    pub by_currency: Vec<CurrencySummary>,
}

fn default_currency() -> String {
    "USD".to_owned()
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
