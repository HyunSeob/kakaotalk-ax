use std::path::Path;

use crate::error::{KaxError, Result};

pub fn validate_title(value: &str) -> Result<&str> {
    if value.trim().is_empty() || value.len() > 4096 {
        return Err(KaxError::validation(
            "An exact window_title of 1..4096 UTF-8 bytes is required",
        ));
    }
    Ok(value)
}

pub fn validate_read_title(value: &str) -> Result<&str> {
    if value.trim().is_empty() || value.len() > 4096 {
        return Err(KaxError::validation(
            "An exact open window_title of 1..4096 UTF-8 bytes is required",
        ));
    }
    Ok(value)
}

pub fn validate_integer(value: i64, name: &str, low: i64, high: i64) -> Result<i64> {
    if !(low..=high).contains(&value) {
        return Err(KaxError::validation(format!(
            "{name} must be an integer between {low} and {high}"
        )));
    }
    Ok(value)
}

/// A calendar date `YYYY-MM-DD` (compared as text with the ISO dates the reads report).
pub fn validate_iso_date<'a>(value: &'a str, name: &str) -> Result<&'a str> {
    let b = value.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let ok = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && digits(0..4)
        && digits(5..7)
        && digits(8..10)
        && (1..=12).contains(&value[5..7].parse::<u32>().unwrap_or(0))
        && (1..=31).contains(&value[8..10].parse::<u32>().unwrap_or(0));
    if !ok {
        return Err(KaxError::validation(format!("{name} must be a date YYYY-MM-DD")));
    }
    Ok(value)
}

pub fn validate_query_text(value: &str, allow_empty: bool) -> Result<&str> {
    if (!allow_empty && value.trim().is_empty()) || value.len() > 4096 {
        return Err(KaxError::validation(
            "query must be a string of at most 4096 UTF-8 bytes",
        ));
    }
    Ok(value)
}

pub fn validate_operation_id(operation_id: &str) -> Result<&str> {
    let valid = !operation_id.is_empty()
        && operation_id.len() <= 128
        && operation_id.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-')
        });
    if !valid {
        return Err(KaxError::validation(
            "operation_id must be 1..128 letters, digits, dots, underscores, colons or hyphens",
        ));
    }
    Ok(operation_id)
}

pub fn validate_wait_seconds(value: f64) -> Result<f64> {
    if !value.is_finite() || !(0.0..=20.0).contains(&value) {
        return Err(KaxError::validation("wait_seconds must be between 0 and 20"));
    }
    Ok(value)
}

pub fn validate_send_text(text: &str) -> Result<&str> {
    if text.trim().is_empty() || text.len() > 16_000 {
        return Err(KaxError::validation(
            "text must be non-empty and at most 16,000 UTF-8 bytes",
        ));
    }
    Ok(text)
}

pub fn is_valid_snapshot_id(value: &str) -> bool {
    value.len() == 24
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn validate_snapshot_id_for_read(snapshot_id: &str, offset: i64) -> Result<()> {
    if !snapshot_id.is_empty() && !is_valid_snapshot_id(snapshot_id) {
        return Err(KaxError::validation(
            "snapshot_id must be the ID returned by an AX read",
        ));
    }
    if offset > 0 && snapshot_id.is_empty() {
        return Err(KaxError::validation(
            "snapshot_id is required for offset > 0; read the first page again",
        ));
    }
    Ok(())
}

pub fn validate_snapshot_id_required(snapshot_id: &str) -> Result<()> {
    if !is_valid_snapshot_id(snapshot_id) {
        return Err(KaxError::validation(
            "A current read_messages snapshot_id is required",
        ));
    }
    Ok(())
}

pub fn validate_snapshot_id_win32(snapshot_id: &str, required: bool) -> Result<()> {
    if (required || !snapshot_id.is_empty()) && !is_valid_snapshot_id(snapshot_id) {
        return Err(KaxError::validation(
            "A current read_messages snapshot_id is required",
        ));
    }
    Ok(())
}

pub fn validate_output_path(output_path: Option<&str>) -> Result<()> {
    if let Some(p) = output_path {
        let path = Path::new(p);
        if !path.is_absolute() {
            return Err(KaxError::validation("output_path must be an absolute path"));
        }
        if path.exists() {
            return Err(KaxError::validation("output_path already exists"));
        }
    }
    Ok(())
}

pub fn validate_win_text(value: &str) -> Result<&str> {
    if value.contains('\0') {
        return Err(KaxError::validation(
            "Win32 text fields do not accept NUL characters",
        ));
    }
    Ok(value)
}
