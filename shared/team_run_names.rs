// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use sha2::{Digest, Sha256};

/// Leave room for the 32-hex run nonce and the sandbox namespace's `kars-`.
pub fn prefix(team: &str) -> String {
    let stem = if team.len() <= 21
        && team
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        team.to_owned()
    } else {
        let readable: String = team
            .chars()
            .take(4)
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let hash = format!("{:x}", Sha256::digest(team.as_bytes()));
        format!("{}-{}", readable.trim_matches('-'), &hash[..16])
            .trim_start_matches('-')
            .to_owned()
    };
    format!("{stem}-run-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_run_names_fit_namespaces_and_preserve_short_names() {
        assert_eq!(prefix("feedback-pilot"), "feedback-pilot-run-");
        for team in [
            "volunteer-feedback-reviewed-team".into(),
            "x".repeat(63),
            "a.b".into(),
        ] {
            let run = format!("{}{}", prefix(&team), "f".repeat(32));
            assert!(format!("kars-{run}").len() <= 63);
            assert!(
                run.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            );
        }
        assert_ne!(
            prefix("volunteer-feedback-reviewed-team-a"),
            prefix("volunteer-feedback-reviewed-team-b")
        );
    }
}
