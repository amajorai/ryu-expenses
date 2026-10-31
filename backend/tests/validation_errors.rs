//! Filter, currency and patch validation, through the validators and the store.

use ryu_expenses::{models::*, store::ExpenseStore, validation::*};

fn valid_new() -> NewExpense {
    NewExpense {
        spent_on: "2026-08-24".to_owned(),
        merchant: "Morning Coffee".to_owned(),
        amount_minor: 650,
        currency: "USD".to_owned(),
        category: "Food".to_owned(),
        account: None,
        note: None,
    }
}

fn root_validation_error(error: &anyhow::Error) -> Option<&ValidationError> {
    error.downcast_ref::<ValidationError>()
}

#[test]
fn a_range_ending_before_it_starts_is_invalid() {
    assert_eq!(
        validate_filters(Some("2026-08-24"), Some("2026-08-23"), None, None),
        Err(ValidationError::InvalidRange)
    );
    assert_eq!(
        validate_filters(Some("2026-08-24"), Some("2026-08-24"), None, None),
        Ok(())
    );
}

#[tokio::test]
async fn the_store_rejects_an_inverted_range_for_list_and_summary() {
    let store = ExpenseStore::open_memory().unwrap();
    store.insert(valid_new()).await.unwrap();
    let filters = ExpenseFilters {
        from: Some("2026-09-01".to_owned()),
        to: Some("2026-08-01".to_owned()),
        ..Default::default()
    };
    let list = store.list(filters.clone()).await.unwrap_err();
    assert_eq!(
        root_validation_error(&list),
        Some(&ValidationError::InvalidRange)
    );
    let summary = store.summary(filters).await.unwrap_err();
    assert_eq!(
        root_validation_error(&summary),
        Some(&ValidationError::InvalidRange)
    );
}

#[test]
fn an_overlong_merchant_or_note_is_too_long() {
    let mut merchant = valid_new();
    merchant.merchant = "m".repeat(161);
    assert_eq!(
        validate_new(&merchant),
        Err(ValidationError::TooLong { field: "merchant" })
    );
    merchant.merchant = "m".repeat(160);
    assert_eq!(validate_new(&merchant), Ok(()));

    let mut note = valid_new();
    note.note = Some("n".repeat(2_001));
    assert_eq!(
        validate_new(&note),
        Err(ValidationError::TooLong { field: "note" })
    );
    note.note = Some("n".repeat(2_000));
    assert_eq!(validate_new(&note), Ok(()));

    let patch = ExpensePatch {
        note: Some(Some("n".repeat(2_001))),
        ..Default::default()
    };
    assert_eq!(
        validate_patch(&patch),
        Err(ValidationError::TooLong { field: "note" })
    );
}

#[tokio::test]
async fn a_padded_lowercase_currency_is_stored_uppercase_and_filters_case_insensitively() {
    let store = ExpenseStore::open_memory().unwrap();
    let mut input = valid_new();
    input.currency = " usd ".to_owned();
    let stored = store.insert(input).await.unwrap();
    assert_eq!(stored.currency, "USD");
    let mut sgd = valid_new();
    sgd.currency = "SGD".to_owned();
    store.insert(sgd).await.unwrap();

    let found = store
        .list(ExpenseFilters {
            currency: Some("usd".to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, stored.id);
    assert_eq!(found[0].currency, "USD");

    let summary = store
        .summary(ExpenseFilters {
            currency: Some(" usd".to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(summary.record_count, 1);
    assert_eq!(summary.total_minor, Some(650));
}

#[tokio::test]
async fn short_or_non_alphabetic_currencies_are_invalid() {
    assert_eq!(normalize_currency(" usd "), Ok("USD".to_owned()));
    for bad in ["US", "U5D", "USDD", ""] {
        assert_eq!(
            normalize_currency(bad),
            Err(ValidationError::InvalidCurrency),
            "{bad}"
        );
        assert_eq!(
            validate_filters(None, None, None, Some(bad)),
            Err(ValidationError::InvalidCurrency),
            "{bad}"
        );
    }
    let store = ExpenseStore::open_memory().unwrap();
    let mut input = valid_new();
    input.currency = "U5D".to_owned();
    let error = store.insert(input).await.unwrap_err();
    assert_eq!(
        root_validation_error(&error),
        Some(&ValidationError::InvalidCurrency)
    );
    assert_eq!(store.counts().await.unwrap(), 0);
}

#[tokio::test]
async fn an_empty_patch_is_rejected_and_changes_nothing() {
    assert_eq!(
        validate_patch(&ExpensePatch::default()),
        Err(ValidationError::EmptyPatch)
    );
    let store = ExpenseStore::open_memory().unwrap();
    let stored = store.insert(valid_new()).await.unwrap();
    let error = store
        .update(&stored.id, ExpensePatch::default())
        .await
        .unwrap_err();
    assert_eq!(
        root_validation_error(&error),
        Some(&ValidationError::EmptyPatch)
    );
    let listed = store.list(ExpenseFilters::default()).await.unwrap();
    assert_eq!(listed[0], stored);
}
