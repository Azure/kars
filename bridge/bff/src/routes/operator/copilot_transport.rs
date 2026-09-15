// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use reqwest::{Client, ClientBuilder, Method, RequestBuilder};

#[cfg(test)]
const LOOPBACK_HOST: &str = "localhost";

#[derive(Clone, Copy)]
pub(super) enum CopilotEndpoint {
    DeviceCode,
    AccessToken,
    Seat,
    Models,
}

impl CopilotEndpoint {
    fn url(self) -> &'static str {
        match self {
            Self::DeviceCode => "https://github.com/login/device/code",
            Self::AccessToken => "https://github.com/login/oauth/access_token",
            Self::Seat => "https://api.github.com/copilot_internal/v2/token",
            Self::Models => "https://api.githubcopilot.com/models",
        }
    }

    fn method(self) -> Method {
        match self {
            Self::DeviceCode | Self::AccessToken => Method::POST,
            Self::Seat | Self::Models => Method::GET,
        }
    }

    #[cfg(test)]
    fn loopback_url(self) -> &'static str {
        match self {
            Self::DeviceCode => "http://localhost/login/device/code",
            Self::AccessToken => "http://localhost/login/oauth/access_token",
            Self::Seat => "http://localhost/copilot_internal/v2/token",
            Self::Models => "http://localhost/models",
        }
    }
}

/// The client and destinations are owned together: callers cannot substitute
/// a redirect-following client or an operator/upstream-supplied credential URL.
pub(super) struct CopilotClient {
    client: Client,
    #[cfg(test)]
    loopback: bool,
}

impl CopilotClient {
    fn builder() -> ClientBuilder {
        Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
    }

    pub(super) fn github() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Self::builder().build()?,
            #[cfg(test)]
            loopback: false,
        })
    }

    pub(super) fn request(&self, endpoint: CopilotEndpoint) -> RequestBuilder {
        #[cfg(test)]
        if self.loopback {
            return self
                .client
                .request(endpoint.method(), endpoint.loopback_url());
        }
        self.client.request(endpoint.method(), endpoint.url())
    }

    // Only unit-test builds can send synthetic credentials over HTTP. Accept a
    // literal loopback address, never a hostname, environment variable or URL.
    #[cfg(test)]
    pub(super) fn loopback(address: std::net::SocketAddr) -> Self {
        assert!(address.ip().is_loopback() && address.port() != 0);
        Self {
            client: Self::builder()
                .https_only(false)
                .no_proxy()
                // Routing belongs to client construction, never credential-bearing URLs.
                .resolve(LOOPBACK_HOST, address)
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .unwrap(),
            loopback: true,
        }
    }
}

#[cfg(test)]
#[path = "copilot_transport_tests.rs"]
mod tests;
