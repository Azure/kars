// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/// Keep short label values readable and long/non-ASCII resource identities stable.
pub(crate) fn value(input: &str) -> String {
    let valid = input.len() <= 63
        && input
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        && input
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && input
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric);
    if valid {
        return input.to_string();
    }
    format!(
        "id-{}",
        crate::kars_receipt_log::sha256_hex(input.as_bytes())
    )
    .chars()
    .take(63)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_names_fit_label_values_without_colliding() {
        assert_eq!(value("short-name"), "short-name");
        let a = format!("{}a", "long".repeat(35));
        let b = format!("{}b", "long".repeat(35));
        assert!(value(&a).len() <= 63);
        assert_ne!(value(&a), value(&b));
        assert_eq!(value(&a), value(&a));
        assert!(value("資料").is_ascii());
    }
}
