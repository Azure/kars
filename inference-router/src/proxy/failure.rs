// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Failover needs an explicit cause and acceptance boundary, not just `Err`.

use axum::http::StatusCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCategory {
    Configuration,
    Authentication,
    Transport,
    ResponseBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acceptance {
    NotAccepted,
    Accepted(StatusCode),
    Rejected(StatusCode),
    Unknown,
}

#[derive(Debug, thiserror::Error)]
#[error("{category:?} upstream failure ({acceptance:?}): {source}")]
pub struct ForwardFailure {
    pub category: FailureCategory,
    pub acceptance: Acceptance,
    #[source]
    source: anyhow::Error,
}

impl ForwardFailure {
    pub fn configuration(error: impl Into<anyhow::Error>) -> anyhow::Error {
        Self {
            category: FailureCategory::Configuration,
            acceptance: Acceptance::NotAccepted,
            source: error.into(),
        }
        .into()
    }

    pub fn authentication(error: impl Into<anyhow::Error>) -> anyhow::Error {
        Self {
            category: FailureCategory::Authentication,
            acceptance: Acceptance::NotAccepted,
            source: error.into(),
        }
        .into()
    }

    pub fn transport(error: reqwest::Error) -> anyhow::Error {
        if error.is_builder() {
            return Self::configuration(error);
        }
        // Only a failed connection proves the request was not accepted.
        // A timeout/reset after sending could already have started generation.
        let acceptance = if error.is_connect() {
            Acceptance::NotAccepted
        } else {
            Acceptance::Unknown
        };
        Self {
            category: FailureCategory::Transport,
            acceptance,
            source: error.into(),
        }
        .into()
    }

    pub fn response_body(status: StatusCode, error: reqwest::Error) -> anyhow::Error {
        Self {
            category: FailureCategory::ResponseBody,
            acceptance: if status.is_success() {
                Acceptance::Accepted(status)
            } else {
                Acceptance::Rejected(status)
            },
            source: error.into(),
        }
        .into()
    }
}

pub fn retryable_rejection(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

pub fn retryable_failure(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ForwardFailure>()
        .is_some_and(|failure| match (failure.category, failure.acceptance) {
            (FailureCategory::Transport, Acceptance::NotAccepted) => true,
            (FailureCategory::ResponseBody, Acceptance::Rejected(status)) => {
                retryable_rejection(status)
            }
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_failures_retry_only_known_retryable_rejections() {
        for code in [
            200, 201, 204, 400, 401, 403, 404, 422, 429, 500, 502, 503, 504, 599,
        ] {
            let status = StatusCode::from_u16(code).unwrap();
            let acceptance = if status.is_success() {
                Acceptance::Accepted(status)
            } else {
                Acceptance::Rejected(status)
            };
            let error: anyhow::Error = ForwardFailure {
                category: FailureCategory::ResponseBody,
                acceptance,
                source: anyhow::anyhow!("truncated body after headers"),
            }
            .into();
            assert_eq!(
                retryable_failure(&error),
                code == 429 || code >= 500,
                "{code}"
            );
        }
        for category in [
            FailureCategory::Authentication,
            FailureCategory::Configuration,
        ] {
            let error: anyhow::Error = ForwardFailure {
                category,
                acceptance: Acceptance::Rejected(StatusCode::SERVICE_UNAVAILABLE),
                source: anyhow::anyhow!("not an inference rejection"),
            }
            .into();
            assert!(!retryable_failure(&error));
        }
    }

    #[test]
    fn unknown_auth_config_and_accepted_body_errors_cannot_trigger_failover() {
        for error in [
            anyhow::anyhow!("untyped failure"),
            ForwardFailure::authentication(anyhow::anyhow!("missing credential")),
            ForwardFailure::configuration(anyhow::anyhow!("invalid endpoint")),
            ForwardFailure {
                category: FailureCategory::ResponseBody,
                acceptance: Acceptance::Accepted(StatusCode::OK),
                source: anyhow::anyhow!("truncated response"),
            }
            .into(),
            ForwardFailure {
                category: FailureCategory::Transport,
                acceptance: Acceptance::Unknown,
                source: anyhow::anyhow!("timeout after sending"),
            }
            .into(),
        ] {
            assert!(!retryable_failure(&error));
        }
    }
}
