// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::Serialize;

pub const FORMAT_ENV: &str = "KARS_EVAL_REPORT_FORMAT";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    V1,
    V2,
}

impl Format {
    pub fn parse(value: Option<&str>) -> Result<Self, &'static str> {
        match value {
            None | Some("v1") => Ok(Self::V1),
            Some("v2") => Ok(Self::V2),
            Some(_) => Err("unsupported KARS_EVAL_REPORT_FORMAT; expected v1 or v2"),
        }
    }

    pub fn version(self) -> &'static str {
        match self {
            Self::V1 => crate::report::REPORT_SCHEMA_VERSION,
            Self::V2 => "v2",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
pub enum ReplayError {
    #[error("Transport")]
    Transport,
    #[error("Timeout")]
    Timeout,
    #[error("Authentication")]
    Authentication,
    #[error("Upstream")]
    Upstream,
    #[error("Protocol")]
    Protocol,
    #[error("BodyRead")]
    BodyRead,
    #[error("BodyTooLarge")]
    BodyTooLarge,
}

impl ReplayError {
    pub fn request(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else if error.is_builder() {
            Self::Protocol
        } else {
            Self::Transport
        }
    }
}

pub fn exit_code(total: usize, failed: usize, errored: usize) -> u8 {
    if total == 0 || errored > 0 {
        2
    } else if failed > 0 {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_and_exit_precedence_never_hide_inconclusive_work() {
        assert_eq!(Format::parse(None), Ok(Format::V1));
        assert_eq!(Format::parse(Some("v1")), Ok(Format::V1));
        assert_eq!(Format::parse(Some("v2")), Ok(Format::V2));
        assert!(Format::parse(Some("")).is_err());
        assert!(Format::parse(Some("v3")).is_err());
        assert_eq!(exit_code(1, 0, 0), 0);
        assert_eq!(exit_code(1, 1, 0), 1);
        assert_eq!(exit_code(1, 0, 1), 2);
        assert_eq!(exit_code(2, 1, 1), 2);
        assert_eq!(exit_code(0, 0, 0), 2);
    }
}
