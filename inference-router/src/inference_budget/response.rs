// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub fn denial(error: &anyhow::Error) -> Option<Response> {
    if let Some(error) = error.downcast_ref::<super::calendar::Error>() {
        return Some(
            crate::errors::openai_coded(
                StatusCode::SERVICE_UNAVAILABLE,
                error.to_string(),
                "token_accounting_unavailable",
                "token_accounting_unavailable",
            )
            .into_response(),
        );
    }
    let error = error.downcast_ref::<super::client::Error>()?;
    if let Some(refusal) = error.refusal {
        // 429/5xx would trigger native SDK backoff for a request that cannot be
        // admitted. This is a governed request refusal, not provider throttling.
        let mut response = crate::errors::openai_coded(
            StatusCode::BAD_REQUEST,
            format!("Governed inference request refused. {}", refusal.message()),
            "invalid_request_error",
            refusal.code(),
        )
        .into_response();
        response.headers_mut().insert(
            "x-should-retry",
            axum::http::HeaderValue::from_static("false"),
        );
        return Some(response);
    }
    let (status, code) = match error.status {
        Some(429) => (
            StatusCode::TOO_MANY_REQUESTS,
            "inference_budget_unavailable",
        ),
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
                refusal: None,
            }
            .into();
            assert_eq!(denial(&error).unwrap().status().as_u16(), expected);
            assert!(!crate::proxy::failure::retryable_failure(&error));
        }
        assert!(denial(&anyhow::anyhow!("ordinary provider error")).is_none());
    }
}
