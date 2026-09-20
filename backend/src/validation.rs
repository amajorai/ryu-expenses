use chrono::NaiveDate;
use thiserror::Error;

use crate::models::{ExpensePatch, NewExpense};

const MAX_MERCHANT_LEN: usize = 160;
const MAX_CATEGORY_LEN: usize = 80;
const MAX_ACCOUNT_LEN: usize = 120;
const MAX_NOTE_LEN: usize = 2_000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("{field} is required")]
    Required { field: &'static str },
    #[error("{field} is too long")]
    TooLong { field: &'static str },
    #[error("spentOn must be an exact YYYY-MM-DD date")]
    InvalidDate,
    #[error("amountMinor must be a positive integer")]
    InvalidAmount,
    #[error("currency must be three uppercase letters")]
    InvalidCurrency,
    #[error("at least one expense field is required")]
    EmptyPatch,
    #[error("from must not be after to")]
    InvalidRange,
}

pub fn parse_date(value: &str) -> Result<NaiveDate, ValidationError> {
    let date =
        NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| ValidationError::InvalidDate)?;
    if date.format("%Y-%m-%d").to_string() == value {
        Ok(date)
    } else {
        Err(ValidationError::InvalidDate)
    }
}

pub fn normalize_currency(value: &str) -> Result<String, ValidationError> {
    let normalized = value.trim().to_ascii_uppercase();
    if normalized.len() == 3 && normalized.bytes().all(|byte| byte.is_ascii_uppercase()) {
        Ok(normalized)
    } else {
        Err(ValidationError::InvalidCurrency)
    }
}

pub fn validate_new(input: &NewExpense) -> Result<(), ValidationError> {
    parse_date(input.spent_on.trim())?;
    validate_text("merchant", &input.merchant, MAX_MERCHANT_LEN)?;
    if input.amount_minor <= 0 {
        return Err(ValidationError::InvalidAmount);
    }
    normalize_currency(&input.currency)?;
    validate_text("category", &input.category, MAX_CATEGORY_LEN)?;
    validate_optional_text("account", input.account.as_deref(), MAX_ACCOUNT_LEN)?;
    validate_optional_text("note", input.note.as_deref(), MAX_NOTE_LEN)?;
    Ok(())
}

pub fn validate_patch(input: &ExpensePatch) -> Result<(), ValidationError> {
    if input.spent_on.is_none()
        && input.merchant.is_none()
        && input.amount_minor.is_none()
        && input.currency.is_none()
        && input.category.is_none()
        && input.account.is_none()
        && input.note.is_none()
    {
        return Err(ValidationError::EmptyPatch);
    }
    if let Some(value) = input.spent_on.as_deref() {
        parse_date(value.trim())?;
    }
    if let Some(value) = input.merchant.as_deref() {
        validate_text("merchant", value, MAX_MERCHANT_LEN)?;
    }
    if input.amount_minor.is_some_and(|value| value <= 0) {
        return Err(ValidationError::InvalidAmount);
    }
    if let Some(value) = input.currency.as_deref() {
        normalize_currency(value)?;
    }
    if let Some(value) = input.category.as_deref() {
        validate_text("category", value, MAX_CATEGORY_LEN)?;
    }
    validate_optional_text("account", input.account.as_deref(), MAX_ACCOUNT_LEN)?;
    validate_optional_text("note", input.note.as_deref(), MAX_NOTE_LEN)?;
    Ok(())
}

pub fn validate_filters(
    from: Option<&str>,
    to: Option<&str>,
    category: Option<&str>,
    currency: Option<&str>,
) -> Result<(), ValidationError> {
    if let Some(value) = from {
        parse_date(value)?;
    }
    if let Some(value) = to {
        parse_date(value)?;
    }
    if let (Some(from), Some(to)) = (from, to) {
        if from > to {
            return Err(ValidationError::InvalidRange);
        }
    }
    if let Some(value) = category {
        validate_text("category", value, MAX_CATEGORY_LEN)?;
    }
    if let Some(value) = currency {
        normalize_currency(value)?;
    }
    Ok(())
}

pub fn normalized_new(input: NewExpense) -> Result<NewExpense, ValidationError> {
    validate_new(&input)?;
    Ok(NewExpense {
        spent_on: input.spent_on.trim().to_owned(),
        merchant: input.merchant.trim().to_owned(),
        amount_minor: input.amount_minor,
        currency: normalize_currency(&input.currency)?,
        category: input.category.trim().to_owned(),
        account: normalize_optional(input.account),
        note: normalize_optional(input.note),
    })
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn validate_text(field: &'static str, value: &str, max_len: usize) -> Result<(), ValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ValidationError::Required { field });
    }
    if trimmed.chars().count() > max_len {
        return Err(ValidationError::TooLong { field });
    }
    Ok(())
}

fn validate_optional_text(
    field: &'static str,
    value: Option<&str>,
    max_len: usize,
) -> Result<(), ValidationError> {
    if let Some(value) = value {
        if value.trim().chars().count() > max_len {
            return Err(ValidationError::TooLong { field });
        }
    }
    Ok(())
}
