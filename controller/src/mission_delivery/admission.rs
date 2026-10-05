// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Error, Result, annotation};
use crate::kars_task::KarsTask;
use base64::{
    Engine, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn digest(content: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(content.as_bytes()))
}

// ECMAScript String.trim() defines the dispatcher's blank-objective test.
fn blank(content: &str) -> bool {
    content.chars().all(|c| {
        matches!(c,
            '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
            '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
            '\u{205f}' | '\u{3000}' | '\u{feff}'
        )
    })
}

fn objective(task: &KarsTask, nonce: &str) -> Result<String> {
    if annotation(&task.metadata, "kars.azure.com/run-objective-nonce") == Some(nonce) {
        let encoded = annotation(&task.metadata, "kars.azure.com/run-objective-b64")
            .ok_or(Error::Invalid("missing bound mission objective"))?;
        // Match the dispatcher's padded standard-base64 language, including unused trailing bits.
        let engine = GeneralPurpose::new(
            &alphabet::STANDARD,
            GeneralPurposeConfig::new()
                .with_decode_padding_mode(DecodePaddingMode::RequireCanonical)
                .with_decode_allow_trailing_bits(true),
        );
        let bytes = engine
            .decode(encoded)
            .map_err(|_| Error::Invalid("invalid bound mission objective encoding"))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::Invalid("invalid bound mission objective UTF-8"))?;
        if blank(&text)
            || annotation(&task.metadata, "kars.azure.com/run-objective-digest")
                != Some(digest(&text).as_str())
        {
            return Err(Error::Invalid("bound mission objective digest mismatch"));
        }
        return Ok(text);
    }
    if nonce.starts_with("rev-") || blank(&task.spec.objective) {
        return Err(Error::Invalid(
            "missing mission objective for requested run",
        ));
    }
    Ok(task.spec.objective.clone())
}

/// Completion and ACK annotations intentionally do not change the installed admission.
pub(super) fn for_task(task: &KarsTask) -> Result<Value> {
    let generation = task
        .metadata
        .generation
        .filter(|g| (1..=9_007_199_254_740_991).contains(g))
        .ok_or(Error::Invalid("missing or invalid mission Task generation"))?;
    let mut admission = json!({"version":1,"state":"idle","taskGeneration":generation,
        "authorizationDigest":task.envelope_digest()});
    if let Some(nonce) = annotation(&task.metadata, "kars.azure.com/run-requested") {
        if nonce.is_empty()
            || nonce.len() > 253
            || !nonce.as_bytes()[0].is_ascii_alphanumeric()
            || !nonce
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err(Error::Invalid("invalid requested mission nonce"));
        }
        admission["state"] = "run".into();
        admission["runNonce"] = nonce.into();
        admission["objectiveDigest"] = digest(&objective(task, nonce)?).into();
    }
    Ok(admission)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    fn task(nonce: Option<&str>) -> KarsTask {
        let mut task = KarsTask::new("writer", Default::default());
        task.metadata.generation = Some(1);
        task.spec.objective = "Write a useful briefing".into();
        task.metadata.annotations = Some(Default::default());
        if let Some(nonce) = nonce {
            set(&mut task, "run-requested", nonce);
        }
        task
    }

    fn set(task: &mut KarsTask, key: &str, value: &str) {
        task.metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(format!("kars.azure.com/{key}"), value.into());
    }

    fn bound(text: &str) -> KarsTask {
        let mut task = task(Some("rev-1"));
        set(&mut task, "run-objective-nonce", "rev-1");
        set(&mut task, "run-objective-b64", &STANDARD.encode(text));
        set(&mut task, "run-objective-digest", &digest(text));
        task
    }

    #[test]
    fn idle_and_run_bind_generation_and_authorization_without_trimming() {
        let idle = task(None);
        assert_eq!(
            for_task(&idle).unwrap(),
            json!({"version":1,"state":"idle",
            "taskGeneration":1,"authorizationDigest":idle.envelope_digest()})
        );
        let text = "\u{feff} Briefing\r\nCafé — 日本語 🌍 \n";
        let revision = bound(text);
        assert_eq!(objective(&revision, "rev-1").unwrap(), text);
        assert_eq!(
            for_task(&revision).unwrap(),
            json!({"version":1,"state":"run",
            "taskGeneration":1,"authorizationDigest":revision.envelope_digest(),
            "runNonce":"rev-1","objectiveDigest":digest(text)})
        );
        let initial = task(Some("initial-run"));
        assert_eq!(
            for_task(&initial).unwrap()["objectiveDigest"],
            digest(&initial.spec.objective)
        );
    }

    #[test]
    fn generation_and_nonce_use_protocol_bounds() {
        for generation in [None, Some(0), Some(-1), Some(9_007_199_254_740_992)] {
            let mut t = task(None);
            t.metadata.generation = generation;
            assert!(for_task(&t).is_err());
        }
        let mut t = task(Some(&"a".repeat(253)));
        t.metadata.generation = Some(9_007_199_254_740_991);
        assert!(for_task(&t).is_ok());
        for nonce in ["", "-run", "run/1", "rún", "run\n", &"a".repeat(254)] {
            set(&mut t, "run-requested", nonce);
            assert!(for_task(&t).is_err(), "{nonce:?}");
        }
        set(&mut t, "run-requested", "A0._:-z");
        assert!(for_task(&t).is_ok());
    }

    #[test]
    fn blank_objectives_match_javascript_trim_but_preserve_nonblank_bytes() {
        for text in [
            "",
            " \t\r\n",
            "\u{feff}",
            "\u{00a0}\u{1680}\u{2000}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}",
        ] {
            assert!(for_task(&bound(text)).is_err());
            let mut t = task(Some("initial"));
            t.spec.objective = text.into();
            assert!(for_task(&t).is_err());
        }
        for text in ["\u{0085}", "\u{200b}", "\u{feff} x \r\n"] {
            assert_eq!(objective(&bound(text), "rev-1").unwrap(), text);
            let mut t = task(Some("initial"));
            t.spec.objective = text.into();
            assert_eq!(objective(&t, "initial").unwrap(), text);
        }
    }

    #[test]
    fn bound_objectives_require_matching_nonce_digest_and_utf8() {
        for key in ["run-objective-b64", "run-objective-digest"] {
            let mut t = bound("x");
            t.metadata
                .annotations
                .as_mut()
                .unwrap()
                .remove(&format!("kars.azure.com/{key}"));
            assert!(for_task(&t).is_err());
        }
        let mut t = bound("x");
        set(&mut t, "run-objective-nonce", "rev-2");
        assert!(for_task(&t).is_err());
        set(&mut t, "run-objective-nonce", "rev-1");
        set(&mut t, "run-objective-digest", &digest("y"));
        assert!(for_task(&t).is_err());
        set(&mut t, "run-objective-b64", "/w==");
        set(&mut t, "run-objective-digest", &digest("\u{fffd}"));
        assert!(for_task(&t).is_err());
        assert!(for_task(&task(Some("rev-unbound"))).is_err());
    }

    #[test]
    fn base64_matches_padded_standard_dispatcher_language() {
        let mut t = bound("x");
        for encoded in ["eA==", "eB==", "eP=="] {
            set(&mut t, "run-objective-b64", encoded);
            assert_eq!(objective(&t, "rev-1").unwrap(), "x");
        }
        for encoded in [
            "eA", "eA=", "eA===", " eA==", "eA==\n", "e_==", "e-==", "eA==eA==", "!",
        ] {
            set(&mut t, "run-objective-b64", encoded);
            assert!(for_task(&t).is_err(), "{encoded:?}");
        }
    }

    #[test]
    fn bound_run_objective_is_independent_of_task_spec_authorization() {
        let original_task = bound("Write the first revision.\n");
        let changed_task = bound("Write a different revision.\n");
        let original = for_task(&original_task).unwrap();
        let changed = for_task(&changed_task).unwrap();
        for field in ["authorizationDigest", "taskGeneration", "runNonce"] {
            assert_eq!(original[field], changed[field]);
        }
        assert_ne!(original["objectiveDigest"], changed["objectiveDigest"]);
        assert_eq!(
            changed["objectiveDigest"],
            digest("Write a different revision.\n")
        );
    }

    #[test]
    fn completion_is_stable_but_authority_run_objective_and_generation_change_admission() {
        let mut t = task(Some("initial"));
        let original = for_task(&t).unwrap();
        for key in ["run-ack", "run-completed"] {
            set(&mut t, key, "initial");
            assert_eq!(for_task(&t).unwrap(), original);
        }
        let mut changed = t.clone();
        changed.metadata.generation = Some(2);
        assert_ne!(for_task(&changed).unwrap(), original);
        changed = t.clone();
        changed.spec.envelope.delegation_depth += 1;
        assert_ne!(
            for_task(&changed).unwrap()["authorizationDigest"],
            original["authorizationDigest"]
        );
        changed = t.clone();
        changed.spec.objective.push('!');
        assert_ne!(
            for_task(&changed).unwrap()["authorizationDigest"],
            original["authorizationDigest"]
        );
        assert_ne!(
            for_task(&changed).unwrap()["objectiveDigest"],
            original["objectiveDigest"]
        );
        changed = t.clone();
        changed.spec.objective.push('\n');
        assert_eq!(changed.envelope_digest(), t.envelope_digest());
        assert_ne!(
            for_task(&changed).unwrap()["objectiveDigest"],
            original["objectiveDigest"]
        );
        set(&mut t, "run-requested", "next");
        assert_ne!(for_task(&t).unwrap(), original);
    }
}
