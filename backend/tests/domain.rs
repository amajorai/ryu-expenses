use ryu_expenses::{models::*, store::ExpenseStore, validation::*};

fn valid_new() -> NewExpense {
    NewExpense {
        spent_on: "2026-08-24".to_owned(),
        merchant: "Morning Coffee".to_owned(),
        amount_minor: 650,
        currency: "USD".to_owned(),
        category: "Food".to_owned(),
        account: Some("Everyday".to_owned()),
        note: None,
    }
}

#[tokio::test]
async fn stores_and_summarizes_one_expense() {
    let store = ExpenseStore::open_memory().expect("in-memory store");
    let expense = store.insert(valid_new()).await.expect("insert expense");

    assert_eq!(expense.merchant, "Morning Coffee");
    assert_eq!(
        store.list(ExpenseFilters::default()).await.unwrap().len(),
        1
    );
    let summary = store
        .summary(ExpenseFilters::default())
        .await
        .expect("summary");
    assert_eq!(summary.record_count, 1);
    assert_eq!(summary.by_currency[0].total_minor, 650);
}

#[tokio::test]
async fn filters_order_and_updates_expenses_without_overwriting_omitted_fields() {
    let store = ExpenseStore::open_memory().expect("in-memory store");
    let first = store.insert(valid_new()).await.unwrap();
    let mut second_input = valid_new();
    second_input.spent_on = "2026-08-23".to_owned();
    second_input.merchant = "Train".to_owned();
    second_input.amount_minor = 1200;
    second_input.category = "Transport".to_owned();
    store.insert(second_input).await.unwrap();

    let food = store
        .list(ExpenseFilters {
            category: Some("Food".to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(food.len(), 1);
    assert_eq!(food[0].id, first.id);

    let updated = store
        .update(
            &first.id,
            ExpensePatch {
                merchant: Some("Corner Cafe".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.merchant, "Corner Cafe");
    assert_eq!(updated.amount_minor, 650);
    assert_eq!(updated.category, "Food");
}

#[tokio::test]
async fn patch_distinguishes_omitted_optional_fields_from_explicit_clear() {
    let store = ExpenseStore::open_memory().expect("in-memory store");
    let mut input = valid_new();
    input.account = Some("Everyday".to_owned());
    input.note = Some("Keep the receipt".to_owned());
    let expense = store.insert(input).await.expect("insert expense");

    let kept = store
        .update(
            &expense.id,
            ExpensePatch {
                merchant: Some("Corner Cafe".to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("update while retaining optional fields");
    assert_eq!(kept.account.as_deref(), Some("Everyday"));
    assert_eq!(kept.note.as_deref(), Some("Keep the receipt"));

    let cleared = store
        .update(
            &expense.id,
            ExpensePatch {
                account: Some(None),
                note: Some(None),
                ..Default::default()
            },
        )
        .await
        .expect("clear optional fields");
    assert_eq!(cleared.account, None);
    assert_eq!(cleared.note, None);
}

#[test]
fn patch_json_distinguishes_omitted_optional_fields_from_explicit_null() {
    let omitted: ExpensePatch = serde_json::from_value(serde_json::json!({
        "merchant": "Corner Cafe"
    }))
    .expect("omitted optional fields parse");
    assert_eq!(omitted.account, None);
    assert_eq!(omitted.note, None);

    let cleared: ExpensePatch = serde_json::from_value(serde_json::json!({
        "account": null,
        "note": null
    }))
    .expect("explicit null fields parse");
    assert_eq!(cleared.account, Some(None));
    assert_eq!(cleared.note, Some(None));
}

#[tokio::test]
async fn deleting_a_missing_expense_is_not_silent() {
    let store = ExpenseStore::open_memory().expect("in-memory store");
    let error = store
        .delete("missing")
        .await
        .expect_err("missing delete must fail");
    assert!(error.to_string().contains("not found"));
}

#[test]
fn validation_rejects_zero_money_and_non_iso_dates() {
    let mut invalid = valid_new();
    invalid.amount_minor = 0;
    assert!(validate_new(&invalid).is_err());
    assert!(parse_date("2026-2-4").is_err());
}

#[tokio::test]
async fn summaries_never_add_different_currencies() {
    let store = ExpenseStore::open_memory().expect("in-memory store");
    store.insert(valid_new()).await.unwrap();
    let mut sgd = valid_new();
    sgd.currency = "SGD".to_owned();
    sgd.amount_minor = 900;
    store.insert(sgd).await.unwrap();

    let summary = store
        .summary(ExpenseFilters::default())
        .await
        .expect("summary");
    assert_eq!(summary.record_count, 2);
    assert_eq!(summary.total_minor, None);
    assert_eq!(summary.by_currency.len(), 2);
}

#[tokio::test]
async fn summaries_cover_every_matching_expense_beyond_the_list_cap() {
    let store = ExpenseStore::open_memory().expect("in-memory store");
    let rows = usize::try_from(ryu_expenses::store::MAX_LIMIT).unwrap() + 50;
    for _ in 0..rows {
        store.insert(valid_new()).await.unwrap();
    }

    let summary = store
        .summary(ExpenseFilters {
            limit: Some(10),
            ..Default::default()
        })
        .await
        .expect("summary");
    assert_eq!(summary.record_count, rows);
    assert_eq!(
        summary.total_minor,
        Some(650 * i64::try_from(rows).unwrap())
    );
    assert_eq!(summary.by_currency[0].categories.len(), 1);
}
