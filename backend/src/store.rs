use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Row};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::models::{
    CategoryTotal, CurrencySummary, Expense, ExpenseFilters, ExpensePatch, ExpenseSummary,
    NewExpense,
};
use crate::validation::{normalize_currency, normalized_new, validate_filters, validate_patch};

pub const DEFAULT_LIMIT: i64 = 200;
pub const MAX_LIMIT: i64 = 500;

#[derive(Clone)]
pub struct ExpenseStore {
    conn: Arc<Mutex<Connection>>,
}

impl ExpenseStore {
    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dir for {}", path.display()))?;
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("opening expenses db at {}", path.display()))?;
        Self::from_connection(conn)
    }

    pub fn open_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
			 PRAGMA synchronous = NORMAL;
			 PRAGMA busy_timeout = 5000;
			 CREATE TABLE IF NOT EXISTS expenses (
			   id TEXT PRIMARY KEY,
			   spent_on TEXT NOT NULL,
			   merchant TEXT NOT NULL,
			   amount_minor INTEGER NOT NULL,
			   currency TEXT NOT NULL,
			   category TEXT NOT NULL,
			   account TEXT,
			   note TEXT,
			   created_at INTEGER NOT NULL,
			   updated_at INTEGER NOT NULL
			 );
			 CREATE INDEX IF NOT EXISTS idx_expenses_spent_on
			   ON expenses(spent_on DESC, created_at DESC);
			 CREATE INDEX IF NOT EXISTS idx_expenses_category
			   ON expenses(category, spent_on DESC);",
        )
        .context("creating expense tracker schema")?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn insert(&self, input: NewExpense) -> Result<Expense> {
        let input = normalized_new(input)?;
        let id = Uuid::new_v4().to_string();
        let now = crate::models::now_ms();
        let conn = self.conn.lock().await;
        conn.execute(
			"INSERT INTO expenses
			 (id, spent_on, merchant, amount_minor, currency, category, account, note, created_at, updated_at)
			 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
			params![
				id,
				input.spent_on,
				input.merchant,
				input.amount_minor,
				input.currency,
				input.category,
				input.account,
				input.note,
				now,
				now,
			],
		)?;
        self.get_locked(&conn, &id)?
            .context("inserted expense disappeared")
    }

    pub async fn list(&self, filters: ExpenseFilters) -> Result<Vec<Expense>> {
        validate_filters(
            filters.from.as_deref(),
            filters.to.as_deref(),
            filters.category.as_deref(),
            filters.currency.as_deref(),
        )?;
        let limit = filters.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let mut sql = String::from(
			"SELECT id, spent_on, merchant, amount_minor, currency, category, account, note, created_at, updated_at
			 FROM expenses WHERE 1 = 1",
		);
        let mut values = Vec::new();
        append_filters(&mut sql, &mut values, &filters);
        sql.push_str(" ORDER BY spent_on DESC, created_at DESC LIMIT ?");
        values.push(Value::Integer(limit));
        let conn = self.conn.lock().await;
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), row_to_expense)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub async fn update(&self, id: &str, patch: ExpensePatch) -> Result<Expense> {
        validate_patch(&patch)?;
        let conn = self.conn.lock().await;
        let current = self
            .get_locked(&conn, id)?
            .ok_or_else(|| anyhow::anyhow!("expense not found"))?;
        let next = NewExpense {
            spent_on: patch.spent_on.unwrap_or(current.spent_on),
            merchant: patch.merchant.unwrap_or(current.merchant),
            amount_minor: patch.amount_minor.unwrap_or(current.amount_minor),
            currency: patch.currency.unwrap_or(current.currency),
            category: patch.category.unwrap_or(current.category),
            account: patch.account.or(current.account),
            note: patch.note.or(current.note),
        };
        let next = normalized_new(next)?;
        let now = crate::models::now_ms();
        conn.execute(
            "UPDATE expenses SET spent_on = ?, merchant = ?, amount_minor = ?, currency = ?,
			 category = ?, account = ?, note = ?, updated_at = ? WHERE id = ?",
            params![
                next.spent_on,
                next.merchant,
                next.amount_minor,
                next.currency,
                next.category,
                next.account,
                next.note,
                now,
                id,
            ],
        )?;
        self.get_locked(&conn, id)?
            .context("updated expense disappeared")
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let changed = conn.execute("DELETE FROM expenses WHERE id = ?", params![id])?;
        if changed == 0 {
            return Err(anyhow::anyhow!("expense not found"));
        }
        Ok(())
    }

    pub async fn summary(&self, mut filters: ExpenseFilters) -> Result<ExpenseSummary> {
        filters.limit = Some(MAX_LIMIT);
        let expenses = self.list(filters).await?;
        let mut by_currency: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
        for expense in &expenses {
            *by_currency
                .entry(expense.currency.clone())
                .or_default()
                .entry(expense.category.clone())
                .or_default() += expense.amount_minor;
        }
        let summaries = by_currency
            .into_iter()
            .map(|(currency, categories)| {
                let mut categories = categories
                    .into_iter()
                    .map(|(category, total_minor)| CategoryTotal {
                        category,
                        total_minor,
                    })
                    .collect::<Vec<_>>();
                categories.sort_by(|left, right| {
                    right
                        .total_minor
                        .cmp(&left.total_minor)
                        .then_with(|| left.category.cmp(&right.category))
                });
                CurrencySummary {
                    total_minor: categories.iter().map(|category| category.total_minor).sum(),
                    currency,
                    categories,
                }
            })
            .collect::<Vec<_>>();
        let total_minor = (summaries.len() == 1).then(|| summaries[0].total_minor);
        Ok(ExpenseSummary {
            record_count: expenses.len(),
            total_minor,
            by_currency: summaries,
        })
    }

    pub async fn counts(&self) -> Result<usize> {
        let conn = self.conn.lock().await;
        let count = conn.query_row("SELECT COUNT(*) FROM expenses", [], |row| {
            row.get::<_, i64>(0)
        })?;
        usize::try_from(count).context("expense count did not fit usize")
    }

    fn get_locked(&self, conn: &Connection, id: &str) -> Result<Option<Expense>> {
        conn.query_row(
			"SELECT id, spent_on, merchant, amount_minor, currency, category, account, note, created_at, updated_at
			 FROM expenses WHERE id = ?",
			params![id],
			row_to_expense,
		)
		.optional()
		.map_err(Into::into)
    }
}

fn append_filters(sql: &mut String, values: &mut Vec<Value>, filters: &ExpenseFilters) {
    if let Some(from) = filters.from.as_deref() {
        sql.push_str(" AND spent_on >= ?");
        values.push(Value::Text(from.to_owned()));
    }
    if let Some(to) = filters.to.as_deref() {
        sql.push_str(" AND spent_on <= ?");
        values.push(Value::Text(to.to_owned()));
    }
    if let Some(category) = filters.category.as_deref() {
        sql.push_str(" AND category = ?");
        values.push(Value::Text(category.trim().to_owned()));
    }
    if let Some(currency) = filters.currency.as_deref() {
        sql.push_str(" AND currency = ?");
        values.push(Value::Text(
            normalize_currency(currency).unwrap_or_else(|_| currency.to_owned()),
        ));
    }
}

fn row_to_expense(row: &Row<'_>) -> rusqlite::Result<Expense> {
    Ok(Expense {
        id: row.get(0)?,
        spent_on: row.get(1)?,
        merchant: row.get(2)?,
        amount_minor: row.get(3)?,
        currency: row.get(4)?,
        category: row.get(5)?,
        account: row.get(6)?,
        note: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}
