//! The expense ledger (`expenses.db`).
//!
//! At rest (see [`crate::seal_keys`]): `merchant`, `account`, `note` and
//! `amount_minor` are sealed per value (`encb:v1:`, store `expenses.db`, bound
//! to the expense id) under the `@ryu/expenses` plugin subkey. `spent_on`,
//! `category` and `currency` stay plaintext: they are the SQL filters and the
//! sort key. Totals are summed in Rust after opening. Legacy plaintext rows are
//! sealed when the store opens (cursor-paginated, resumable), then freed pages
//! are scrubbed. Every connection runs with `secure_delete = ON`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Row};
use ryu_crypto::content_sqlite::{
    enable_secure_delete, finish_store, migrate_columns, run_pending_vacuum, ColumnPlan, KeyKind,
};
use ryu_crypto::ContentContext;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::seal_keys::{
    ensure_compat_write, reopen_plaintext_items, sqlite_store_seal, PluginKeys, StoreSeal,
};

use crate::models::{
    CategoryTotal, CurrencySummary, Expense, ExpenseFilters, ExpensePatch, ExpenseSummary,
    NewExpense,
};
use crate::validation::{normalize_currency, normalized_new, validate_filters, validate_patch};

pub const DEFAULT_LIMIT: i64 = 200;
pub const MAX_LIMIT: i64 = 500;

/// The registered content-store name (part of every key derivation; never rename).
pub const STORE: &str = "expenses.db";

/// The sealed columns of `expenses`.
const SEALED_COLUMNS: [&str; 4] = ["merchant", "account", "note", "amount_minor"];

fn ctx(column: &str, id: &str) -> ContentContext {
    ContentContext::for_id(format!("expenses.{column}"), id)
}

/// The sealed columns, for the open-time migration.
pub fn column_plans() -> Result<Vec<ColumnPlan>> {
    SEALED_COLUMNS
        .iter()
        .map(|column| ColumnPlan::text("expenses", column, "id", KeyKind::Text))
        .collect()
}

#[derive(Clone)]
pub struct ExpenseStore {
    conn: Arc<Mutex<Connection>>,
    seal: StoreSeal,
    unreadable: Arc<AtomicU64>,
}

impl ExpenseStore {
    /// Open the store, resolving this process's sealing key with the store's
    /// directory as the plugin data dir (see [`crate::seal_keys`]).
    pub fn open(path: PathBuf) -> Result<Self> {
        let dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let keys = PluginKeys::resolve(crate::state::PLUGIN_ID, &dir)?;
        Self::open_with_keys(path, &keys)
    }

    /// [`Self::open`] with explicit keys.
    pub fn open_with_keys(path: PathBuf, keys: &PluginKeys) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dir for {}", path.display()))?;
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("opening expenses db at {}", path.display()))?;
        Self::from_connection(conn, keys, Some(&path))
    }

    /// An in-memory store (tests). Nothing reaches the disk, so it does not seal.
    pub fn open_memory() -> Result<Self> {
        Self::open_memory_with_keys(&PluginKeys::compat())
    }

    /// An in-memory store that seals under `keys` (tests).
    pub fn open_memory_with_keys(keys: &PluginKeys) -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, keys, None)
    }

    /// The sealing health item: `{state, detail?, unreadable_rows}`.
    pub fn seal_health(&self) -> serde_json::Value {
        let mut health = self.seal.health();
        health["unreadable_rows"] = self.unreadable.load(Ordering::Relaxed).into();
        health
    }

    fn from_connection(
        mut conn: Connection,
        keys: &PluginKeys,
        path: Option<&Path>,
    ) -> Result<Self> {
        enable_secure_delete(&conn)?;
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
        let seal = sqlite_store_seal(keys, &conn, STORE)?;
        if let StoreSeal::Sealing(cipher) = &seal {
            if let Some(path) = path {
                if let Err(error) = run_pending_vacuum(&conn, path, STORE) {
                    tracing::warn!("expenses.db: deferred compaction failed: {error:#}");
                }
            }
            let plans = column_plans()?;
            reopen_plaintext_items(&conn, STORE, &plans)?;
            let report = migrate_columns(&mut conn, cipher, &plans)
                .context("sealing legacy expenses.db rows")?;
            if report.sealed > 0 {
                tracing::info!(count = report.sealed, "expenses.db: sealed legacy values");
                if let Some(path) = path {
                    if let Err(error) = finish_store(&conn, path, STORE) {
                        tracing::warn!("expenses.db: scrubbing freed pages failed: {error:#}");
                    }
                }
            }
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            seal,
            unreadable: Arc::new(AtomicU64::new(0)),
        })
    }

    fn seal_row(&self, id: &str, input: &NewExpense) -> Result<SealedFields> {
        Ok(SealedFields {
            merchant: self.seal.seal_text(&input.merchant, &ctx("merchant", id))?,
            amount_minor: match &self.seal {
                // Compat mode keeps the pre-sealing INTEGER cell.
                StoreSeal::Compat => Value::Integer(input.amount_minor),
                seal => Value::Text(
                    seal.seal_text(&input.amount_minor.to_string(), &ctx("amount_minor", id))?,
                ),
            },
            account: self
                .seal
                .seal_opt(input.account.as_deref(), &ctx("account", id))?,
            note: self
                .seal
                .seal_opt(input.note.as_deref(), &ctx("note", id))?,
        })
    }

    /// The whole-number amount in one stored `amount_minor` cell: sealed text, or
    /// the plain INTEGER of a row written before sealing.
    fn open_amount(&self, id: &str, cell: Value) -> Result<i64> {
        match cell {
            Value::Integer(n) => Ok(n),
            Value::Text(text) => self
                .seal
                .open_text(&text, &ctx("amount_minor", id))?
                .trim()
                .parse()
                .context("an expense amount is not a whole number"),
            Value::Real(r) => Ok(r as i64),
            _ => anyhow::bail!("an expense amount is missing"),
        }
    }

    fn open_row(&self, raw: RawExpense) -> Result<Expense> {
        let id = raw.id;
        let amount_minor = self.open_amount(&id, raw.amount_minor)?;
        Ok(Expense {
            merchant: self.seal.open_text(&raw.merchant, &ctx("merchant", &id))?,
            account: self
                .seal
                .open_opt(raw.account.as_deref(), &ctx("account", &id))?,
            note: self.seal.open_opt(raw.note.as_deref(), &ctx("note", &id))?,
            amount_minor,
            spent_on: raw.spent_on,
            currency: raw.currency,
            category: raw.category,
            created_at: raw.created_at,
            updated_at: raw.updated_at,
            id,
        })
    }

    pub async fn insert(&self, input: NewExpense) -> Result<Expense> {
        let input = normalized_new(input)?;
        let id = Uuid::new_v4().to_string();
        let now = crate::models::now_ms();
        let sealed = self.seal_row(&id, &input)?;
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_compat_write(&self.seal, &tx, STORE)?;
        tx.execute(
			"INSERT INTO expenses
			 (id, spent_on, merchant, amount_minor, currency, category, account, note, created_at, updated_at)
			 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
			params![
				id,
				input.spent_on,
				sealed.merchant,
				sealed.amount_minor,
				input.currency,
				input.category,
				sealed.account,
				sealed.note,
				now,
				now,
			],
		)?;
        tx.commit()?;
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
        let rows = statement
            .query_map(params_from_iter(values), row_to_raw)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for raw in rows {
            let id = raw.id.clone();
            match self.open_row(raw) {
                Ok(expense) => out.push(expense),
                Err(error) => {
                    // One unreadable row never hides the whole ledger.
                    let total = self.unreadable.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::warn!(expense = %id, total, "skipping an expense that does not open: {error:#}");
                }
            }
        }
        Ok(out)
    }

    pub async fn update(&self, id: &str, patch: ExpensePatch) -> Result<Expense> {
        validate_patch(&patch)?;
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_compat_write(&self.seal, &tx, STORE)?;
        let current = self
            .get_locked(&tx, id)?
            .ok_or_else(|| anyhow::anyhow!("expense not found"))?;
        let next = NewExpense {
            spent_on: patch.spent_on.unwrap_or(current.spent_on),
            merchant: patch.merchant.unwrap_or(current.merchant),
            amount_minor: patch.amount_minor.unwrap_or(current.amount_minor),
            currency: patch.currency.unwrap_or(current.currency),
            category: patch.category.unwrap_or(current.category),
            account: patch.account.unwrap_or(current.account),
            note: patch.note.unwrap_or(current.note),
        };
        let next = normalized_new(next)?;
        let now = crate::models::now_ms();
        let sealed = self.seal_row(id, &next)?;
        tx.execute(
            "UPDATE expenses SET spent_on = ?, merchant = ?, amount_minor = ?, currency = ?,
			 category = ?, account = ?, note = ?, updated_at = ? WHERE id = ?",
            params![
                next.spent_on,
                sealed.merchant,
                sealed.amount_minor,
                next.currency,
                next.category,
                sealed.account,
                sealed.note,
                now,
                id,
            ],
        )?;
        tx.commit()?;
        self.get_locked(&conn, id)?
            .context("updated expense disappeared")
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_compat_write(&self.seal, &tx, STORE)?;
        let changed = tx.execute("DELETE FROM expenses WHERE id = ?", params![id])?;
        if changed == 0 {
            return Err(anyhow::anyhow!("expense not found"));
        }
        tx.commit()?;
        Ok(())
    }

    /// Totals over every matching expense. Aggregated in SQL so the result
    /// covers the whole filtered range: `filters.limit` (a list page size) is
    /// deliberately ignored.
    pub async fn summary(&self, filters: ExpenseFilters) -> Result<ExpenseSummary> {
        validate_filters(
            filters.from.as_deref(),
            filters.to.as_deref(),
            filters.category.as_deref(),
            filters.currency.as_deref(),
        )?;
        // Amounts are sealed per value, so SQL cannot sum them: the filters still
        // run in SQL and every matching amount is opened and totalled here.
        let mut sql =
            String::from("SELECT id, currency, category, amount_minor FROM expenses WHERE 1 = 1");
        let mut values = Vec::new();
        append_filters(&mut sql, &mut values, &filters);
        let mut record_count: usize = 0;
        let mut by_currency: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
        {
            let conn = self.conn.lock().await;
            let mut statement = conn.prepare(&sql)?;
            let mut rows = statement.query(params_from_iter(values))?;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let currency: String = row.get(1)?;
                let category: String = row.get(2)?;
                let amount_minor = self.open_amount(&id, row.get(3)?)?;
                record_count = record_count
                    .checked_add(1)
                    .context("expense count overflowed")?;
                let total = by_currency
                    .entry(currency)
                    .or_default()
                    .entry(category)
                    .or_insert(0_i64);
                *total = total
                    .checked_add(amount_minor)
                    .context("expense category total overflowed")?;
            }
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
                let total_minor = categories
                    .iter()
                    .try_fold(0_i64, |sum, category| sum.checked_add(category.total_minor))
                    .with_context(|| format!("{currency} expense total overflowed"))?;
                Ok(CurrencySummary {
                    total_minor,
                    currency,
                    categories,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let total_minor = (summaries.len() == 1).then(|| summaries[0].total_minor);
        Ok(ExpenseSummary {
            record_count,
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
			row_to_raw,
		)
		.optional()?
		.map(|raw| self.open_row(raw))
		.transpose()
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

/// The sealed (or, in compat mode, plaintext) cells of one row.
struct SealedFields {
    merchant: String,
    amount_minor: Value,
    account: Option<String>,
    note: Option<String>,
}

/// One row as stored.
struct RawExpense {
    id: String,
    spent_on: String,
    merchant: String,
    amount_minor: Value,
    currency: String,
    category: String,
    account: Option<String>,
    note: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn row_to_raw(row: &Row<'_>) -> rusqlite::Result<RawExpense> {
    Ok(RawExpense {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal_keys::KeySource;
    use base64::Engine as _;
    use ryu_crypto::{ContentCipher, PluginCipher};

    const SECRET: &str = "zebra-merchant-marker";

    fn keys(n: u8) -> PluginKeys {
        let subkey = base64::engine::general_purpose::STANDARD.encode([n; 32]);
        PluginKeys::sealing(
            PluginCipher::from_subkey_b64(crate::state::PLUGIN_ID, &subkey).unwrap(),
            KeySource::Delivered,
        )
    }

    fn db_path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("ryu-expenses-seal-{}", Uuid::new_v4().simple()))
            .join(STORE)
    }

    fn disk_bytes(path: &Path) -> Vec<u8> {
        let mut bytes = std::fs::read(path).unwrap_or_default();
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        bytes.extend(std::fs::read(PathBuf::from(wal)).unwrap_or_default());
        bytes
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    }

    fn new_expense(merchant: &str, amount_minor: i64) -> NewExpense {
        NewExpense {
            spent_on: "2026-08-24".to_owned(),
            merchant: merchant.to_owned(),
            amount_minor,
            currency: "USD".to_owned(),
            category: "Food".to_owned(),
            account: Some(format!("acct {SECRET}")),
            note: Some(format!("note {SECRET}")),
        }
    }

    fn raw_cells(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT merchant || '|' || amount_minor || '|' || account || '|' || note FROM expenses ORDER BY id")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[tokio::test]
    async fn content_is_sealed_on_disk_and_reads_back() {
        let path = db_path();
        let store = ExpenseStore::open_with_keys(path.clone(), &keys(1)).unwrap();
        let created = store.insert(new_expense(SECRET, 98_765)).await.unwrap();
        assert_eq!(created.merchant, SECRET);
        assert_eq!(created.amount_minor, 98_765);
        let updated = store
            .update(
                &created.id,
                ExpensePatch {
                    amount_minor: Some(12_345),
                    ..ExpensePatch::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.amount_minor, 12_345);
        assert_eq!(updated.merchant, SECRET);
        let summary = store.summary(ExpenseFilters::default()).await.unwrap();
        assert_eq!(summary.total_minor, Some(12_345));
        let bytes = disk_bytes(&path);
        assert!(!contains(&bytes, SECRET));
        let conn = store.conn.lock().await;
        let cipher = keys(1).content_cipher(STORE).unwrap();
        ryu_crypto::content_sqlite::assert_all_open(&conn, &cipher, &column_plans().unwrap())
            .unwrap();
    }

    #[tokio::test]
    async fn legacy_plaintext_is_migrated_once_and_still_reads() {
        let path = db_path();
        {
            let store = ExpenseStore::open_with_keys(path.clone(), &PluginKeys::compat()).unwrap();
            assert_eq!(store.seal_health()["state"], "compat");
            store.insert(new_expense(SECRET, 4_242)).await.unwrap();
        }
        assert!(contains(&disk_bytes(&path), SECRET));
        let store = ExpenseStore::open_with_keys(path.clone(), &keys(2)).unwrap();
        let listed = store.list(ExpenseFilters::default()).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].merchant, SECRET);
        assert_eq!(listed[0].amount_minor, 4_242);
        let first = {
            let conn = store.conn.lock().await;
            let cipher = keys(2).content_cipher(STORE).unwrap();
            ryu_crypto::content_sqlite::assert_all_open(&conn, &cipher, &column_plans().unwrap())
                .unwrap();
            raw_cells(&conn)
        };
        drop(store);
        assert!(!contains(&disk_bytes(&path), SECRET));
        let store = ExpenseStore::open_with_keys(path, &keys(2)).unwrap();
        assert_eq!(raw_cells(&*store.conn.lock().await), first);
    }

    #[tokio::test]
    async fn a_value_copied_to_another_row_does_not_open_and_lists_skip_it() {
        let store = ExpenseStore::open_memory_with_keys(&keys(3)).unwrap();
        let a = store.insert(new_expense("first", 1)).await.unwrap();
        let b = store.insert(new_expense("second", 2)).await.unwrap();
        {
            let conn = store.conn.lock().await;
            conn.execute(
                "UPDATE expenses SET merchant = (SELECT merchant FROM expenses WHERE id = ?1) WHERE id = ?2",
                params![a.id, b.id],
            )
            .unwrap();
        }
        assert!(store.update(&b.id, ExpensePatch::default()).await.is_err());
        let listed = store.list(ExpenseFilters::default()).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, a.id);
        assert_eq!(store.seal_health()["unreadable_rows"], 1);
    }

    #[tokio::test]
    async fn a_keyless_process_stops_writing_once_the_sidecar_seals_the_store() {
        let path = db_path();
        // The keyless MCP process opens a never-sealed store first (compat).
        let keyless = ExpenseStore::open_with_keys(path.clone(), &PluginKeys::compat()).unwrap();
        assert_eq!(keyless.seal_health()["state"], "compat");
        keyless.insert(new_expense("before", 1)).await.unwrap();
        // The sidecar then opens with its key: records its KCV and seals.
        let sidecar = ExpenseStore::open_with_keys(path.clone(), &keys(8)).unwrap();
        assert_eq!(sidecar.seal_health()["state"], "ok");
        // The long-lived keyless process must not write plaintext any more.
        let error = keyless.insert(new_expense(SECRET, 2)).await.unwrap_err();
        assert!(error.to_string().contains("sealed"), "{error}");
        let listed = keyless.list(ExpenseFilters::default()).await.unwrap();
        assert!(listed.iter().all(|expense| expense.merchant != SECRET));
        assert!(keyless
            .delete(&listed.first().map(|e| e.id.clone()).unwrap_or_default())
            .await
            .is_err());
        drop(keyless);
        drop(sidecar);
        assert!(!contains(&disk_bytes(&path), SECRET));
    }

    #[tokio::test]
    async fn plaintext_written_after_sealing_finished_is_sealed_at_next_open() {
        let path = db_path();
        {
            let store = ExpenseStore::open_with_keys(path.clone(), &keys(9)).unwrap();
            store.insert(new_expense("sealed", 3)).await.unwrap();
        }
        {
            // A plaintext row from an older keyless writer, after every item
            // was marked sealed.
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "INSERT INTO expenses (id, spent_on, merchant, amount_minor, currency, category, account, note, created_at, updated_at)
                 VALUES ('late', '2026-08-25', ?1, 77, 'USD', 'Food', NULL, NULL, 0, 0)",
                params![SECRET],
            )
            .unwrap();
        }
        let store = ExpenseStore::open_with_keys(path.clone(), &keys(9)).unwrap();
        let listed = store.list(ExpenseFilters::default()).await.unwrap();
        assert!(listed
            .iter()
            .any(|e| e.merchant == SECRET && e.amount_minor == 77));
        {
            let conn = store.conn.lock().await;
            let cipher = keys(9).content_cipher(STORE).unwrap();
            ryu_crypto::content_sqlite::assert_all_open(&conn, &cipher, &column_plans().unwrap())
                .unwrap();
        }
        drop(store);
        assert!(!contains(&disk_bytes(&path), SECRET));
    }

    #[tokio::test]
    async fn another_key_or_no_key_refuses_writes_and_keeps_data() {
        let path = db_path();
        let store = ExpenseStore::open_with_keys(path.clone(), &keys(4)).unwrap();
        let kept = store.insert(new_expense("kept", 7)).await.unwrap();
        drop(store);
        let wrong = ExpenseStore::open_with_keys(path.clone(), &keys(5)).unwrap();
        assert_eq!(wrong.seal_health()["state"], "unhealthy");
        assert!(wrong.insert(new_expense("x", 1)).await.is_err());
        assert!(wrong.delete(&kept.id).await.is_err());
        drop(wrong);
        let none = ExpenseStore::open_with_keys(path.clone(), &PluginKeys::compat()).unwrap();
        assert_eq!(none.seal_health()["state"], "unhealthy");
        assert!(none.insert(new_expense("x", 1)).await.is_err());
        drop(none);
        let back = ExpenseStore::open_with_keys(path.clone(), &keys(4)).unwrap();
        let listed = back.list(ExpenseFilters::default()).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].merchant, "kept");
        let raw: String = back
            .conn
            .lock()
            .await
            .query_row("SELECT merchant FROM expenses", [], |row| row.get(0))
            .unwrap();
        assert!(ContentCipher::is_sealed_text(&raw));
    }
}
