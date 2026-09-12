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
