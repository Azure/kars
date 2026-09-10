// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub fn denial(error: &anyhow::Error) -> Option<Response> {
    let error = error.downcast_ref::<super::client::Error>()?;
    let (status, code) = match error.status {
        Some(429) => (StatusCode::TOO_MANY_REQUESTS, "inference_budget_exhausted"),
        Some(403) => (StatusCode::FORBIDDEN, "inference_budget_authority"),
        Some(409) => (StatusCode::CONFLICT, "inference_budget_attempt_state"),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "inference_budget_unavailable",
        ),
    };
    Some(crate::errors::openai_coded(status, error.to_string(), code, code).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_errors_are_not_reported_as_provider_bad_gateways() {
        for (code, expected) in [
            (Some(429), 429),
            (Some(403), 403),
            (Some(409), 409),
            (None, 503),
        ] {
            let error: anyhow::Error = super::super::client::Error {
                stage: "/v1/reserve",
                status: code,
            }
            .into();
            assert_eq!(denial(&error).unwrap().status().as_u16(), expected);
            assert!(!crate::proxy::failure::retryable_failure(&error));
        }
        assert!(denial(&anyhow::anyhow!("ordinary provider error")).is_none());
    }
}
