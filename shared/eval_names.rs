// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

pub(crate) fn bounded_name(name: &str, limit: usize) -> String {
    if name.len() <= limit {
        return name.into();
    }
    let suffix = short_hash(name);
    let prefix = name[..limit - suffix.len() - 1].trim_end_matches(['-', '.']);
    format!("{prefix}-{suffix}")
}

pub(crate) fn short_hash(value: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(value.as_bytes());
    digest
        .iter()
        .take(5)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_readers_and_writers_use_stable_bounded_names() {
        assert_eq!(
            bounded_name("karseval-eval-report", 253),
            "karseval-eval-report"
        );
        for limit in [52, 63, 253] {
            assert_eq!(bounded_name(&"a".repeat(limit), limit), "a".repeat(limit));
            let raw = format!("karseval-{}-report", "a".repeat(253));
            let name = bounded_name(&raw, limit);
            assert!(name.len() <= limit);
            assert_eq!(name, bounded_name(&raw, limit));
            assert_ne!(
                name,
                bounded_name(&raw.replace("-report", "-corpus"), limit)
            );
            assert!(name.ends_with(&short_hash(&raw)));
        }
    }
}
