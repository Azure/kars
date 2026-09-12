// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Request-local observations of the unchanged kube-client stack. No HTTP
//! request fields, credentials, endpoints or upstream error text are recorded.

use axum::http::{Request, Response};
use futures::future::BoxFuture;
use kube::{
    Client, Config,
    client::{Body, ClientBuilder},
    core::DynamicObject,
};
use std::{
    fmt,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
    task::{Context, Poll},
};
use tower::{Layer, Service};
use tracing::{
    Subscriber,
    field::{Field, Visit},
    instrument::WithSubscriber,
    span::{Attributes, Id, Record},
};

const INITIALIZED: u16 = 1;
const BUILT: u16 = 2;
const ENTERED: u16 = 4;
const DISPATCH: u16 = 8;
const HEADERS: u16 = 16;
const DECODED: u16 = 32;
const HTTPS: u16 = 64;
const TLS: u16 = 128;
const CA: u16 = 256;
const TOKEN_FILE: u16 = 512;
const PROXY: u16 = 1024;
const ENVIRONMENT: u16 = 2048;
const NAMESPACE: u16 = 4096;
const TCP_STARTED: u16 = 8192;
const TCP_CONNECTED: u16 = 16384;
const HTTP_HANDSHAKE: u16 = 32768;

const TCP_TARGET: &str = "hyper_util::client::legacy::connect::http";
const HTTP_TARGET: &str = "hyper_util::client::legacy::client";

#[derive(Clone)]
pub(super) struct Progress {
    bits: Arc<AtomicU16>,
    status: Arc<AtomicU16>,
    namespace: String,
}

impl Progress {
    pub(super) fn new(namespace: String) -> Self {
        Self {
            bits: Arc::new(AtomicU16::new(0)),
            status: Arc::new(AtomicU16::new(0)),
            namespace,
        }
    }
    fn set(&self, bits: u16) {
        self.bits.fetch_or(bits, Ordering::Relaxed);
    }
    pub(super) fn initialized(&self) {
        self.set(INITIALIZED);
    }
    pub(super) fn built(&self) {
        self.set(BUILT);
    }
    pub(super) fn decoded(&self) {
        self.set(DECODED);
    }
}

pub(super) struct Pending(pub(super) Progress);

pub(super) async fn read_target(
    client: &Client,
    request: Request<Vec<u8>>,
    progress: &Progress,
) -> Result<DynamicObject, kube::Error> {
    // kube-client also logs malformed payloads while collecting/decoding after
    // the service has returned headers. Keep the entire request inside this
    // scope; the caller's bounded Pending diagnostic is deliberately outside.
    client
        .request(request)
        .with_subscriber(tracing::Dispatch::new(HttpBoundary(progress.clone())))
        .await
}

impl Drop for Pending {
    fn drop(&mut self) {
        let bits = self.0.bits.load(Ordering::Relaxed);
        if bits & DECODED != 0 {
            return;
        }
        tracing::warn!(target: "kars_inference_router::observation_privacy",
            stage = "observer_target_client",
            client_initialized = bits & INITIALIZED != 0,
            request_built = bits & BUILT != 0,
            service_entered = bits & ENTERED != 0,
            dispatch_observable = bits & ENTERED != 0
                && tracing::level_filters::STATIC_MAX_LEVEL >= tracing::level_filters::LevelFilter::DEBUG,
            after_auth_dispatch = bits & DISPATCH != 0,
            transport_debug_observable = bits & ENTERED != 0
                && tracing::level_filters::STATIC_MAX_LEVEL >= tracing::level_filters::LevelFilter::DEBUG,
            transport_trace_observable = bits & ENTERED != 0
                && tracing::level_filters::STATIC_MAX_LEVEL >= tracing::level_filters::LevelFilter::TRACE,
            tcp_connect_started = bits & TCP_STARTED != 0,
            tcp_connected = bits & TCP_CONNECTED != 0,
            http_handshake_complete = bits & HTTP_HANDSHAKE != 0,
            response_headers = bits & HEADERS != 0,
            config_observed = bits & ENTERED != 0,
            https = bits & HTTPS != 0,
            tls_verification = bits & TLS != 0,
            root_ca_present = bits & CA != 0,
            token_file_only = bits & TOKEN_FILE != 0,
            proxy_configured = bits & PROXY != 0,
            endpoint_environment_matches = bits & ENVIRONMENT != 0,
            runtime_namespace_matches = bits & NAMESPACE != 0,
            http_status = self.0.status.load(Ordering::Relaxed),
            "Private observation target client pending");
    }
}

#[derive(Clone)]
struct ClientLayer {
    bits: u16,
    namespace: String,
}

pub(super) fn client(config: Config) -> Result<Client, kube::Error> {
    let mut bits = 0;
    if config.cluster_url.scheme_str() == Some("https") {
        bits |= HTTPS;
    }
    if !config.accept_invalid_certs {
        bits |= TLS;
    }
    if config
        .root_cert
        .as_ref()
        .is_some_and(|certs| !certs.is_empty())
    {
        bits |= CA;
    }
    if config.proxy_url.is_some() {
        bits |= PROXY;
    }
    let auth = &config.auth_info;
    if auth.token_file.as_deref() == Some("/var/run/secrets/kubernetes.io/serviceaccount/token")
        && auth.token.is_none()
        && auth.username.is_none()
        && auth.password.is_none()
        && auth.exec.is_none()
        && auth.auth_provider.is_none()
    {
        bits |= TOKEN_FILE;
    }
    let host = std::env::var("KUBERNETES_SERVICE_HOST").ok();
    let port = std::env::var("KUBERNETES_SERVICE_PORT").ok();
    if endpoint_environment_matches(&config.cluster_url, host.as_deref(), port.as_deref()) {
        bits |= ENVIRONMENT;
    }
    let layer = ClientLayer {
        bits,
        namespace: config.default_namespace.clone(),
    };
    Ok(ClientBuilder::try_from(config)?.with_layer(&layer).build())
}

fn endpoint_environment_matches(
    uri: &axum::http::Uri,
    host: Option<&str>,
    port: Option<&str>,
) -> bool {
    let (Some(actual), Some(expected), Some(port)) = (
        uri.host(),
        host,
        port.and_then(|value| value.parse::<u16>().ok()),
    ) else {
        return false;
    };
    // kube-client 3.1 incluster_env omits :443 and canonicalizes IP literals.
    // An absent explicit URI port is not an absent HTTPS destination port.
    if uri.scheme_str() != Some("https") || uri.port_u16().unwrap_or(443) != port {
        return false;
    }
    let ip = |host: &str| {
        host.strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host)
            .parse::<IpAddr>()
    };
    match (ip(actual), ip(expected)) {
        (Ok(actual), Ok(expected)) => actual == expected,
        (Err(_), Err(_)) => actual.eq_ignore_ascii_case(expected),
        _ => false,
    }
}

struct Observed<S> {
    inner: S,
    config: ClientLayer,
}

impl<S> Layer<S> for ClientLayer {
    type Service = Observed<S>;
    fn layer(&self, inner: S) -> Self::Service {
        Observed {
            inner,
            config: self.clone(),
        }
    }
}

impl<S, B> Service<Request<Body>> for Observed<S>
where
    S: Service<Request<Body>, Response = Response<B>>,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context)
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let Some(progress) = request.extensions().get::<Progress>().cloned() else {
            return Box::pin(self.inner.call(request));
        };
        progress.set(
            ENTERED
                | self.config.bits
                | if progress.namespace == self.config.namespace {
                    NAMESPACE
                } else {
                    0
                },
        );
        let dispatch = tracing::Dispatch::new(HttpBoundary(progress.clone()));
        let future = tracing::dispatcher::with_default(&dispatch, || self.inner.call(request));
        Box::pin(async move {
            let result = future.with_subscriber(dispatch).await;
            if let Ok(response) = &result {
                progress.set(HEADERS);
                progress
                    .status
                    .store(response.status().as_u16(), Ordering::Relaxed);
            }
            result
        })
    }
}

// kube-client 3.1's default builder places its HTTP trace span *inside* the
// authentication layer (client/builder.rs). Observing that span proves dispatch
// beyond auth, not a TCP connection or packet delivery. The scoped subscriber
// discards every span field, including URLs and upstream error bodies.
// hyper-util 0.1.20's fixed connector messages distinguish TCP progress from
// HTTP connection setup (after TLS for HTTPS). These are positive-only facts:
// pooled connections can skip them, and detached work is not attributed here.
struct HttpBoundary(Progress);

impl Subscriber for HttpBoundary {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        (metadata.is_span()
            && metadata.name() == "HTTP"
            && metadata.target() == "kube_client::client::builder")
            || (metadata.is_event()
                && ((metadata.target() == TCP_TARGET
                    && *metadata.level() == tracing::Level::DEBUG)
                    || (metadata.target() == HTTP_TARGET
                        && *metadata.level() == tracing::Level::TRACE)))
    }
    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        if self.enabled(attributes.metadata()) {
            self.0.set(DISPATCH);
        }
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if self.enabled(event.metadata()) {
            event.record(&mut TransportMessage {
                progress: &self.0,
                tcp: event.metadata().target() == TCP_TARGET,
            });
        }
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

struct TransportMessage<'a> {
    progress: &'a Progress,
    tcp: bool,
}

impl Visit for TransportMessage<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() != "message" {
            return;
        }
        let bits = if self.tcp {
            if message_starts_with(value, "connecting to ") {
                TCP_STARTED
            } else if message_starts_with(value, "connected to ") {
                TCP_CONNECTED
            } else {
                0
            }
        } else if message_starts_with(
            value,
            "http1 handshake complete, spawning background dispatcher task",
        ) || message_starts_with(
            value,
            "http2 handshake complete, spawning background dispatcher task",
        ) {
            HTTP_HANDSHAKE
        } else {
            0
        };
        self.progress.set(bits);
    }
}

fn message_starts_with(value: &dyn fmt::Debug, expected: &'static str) -> bool {
    struct Prefix {
        remaining: &'static [u8],
        matched: bool,
    }
    impl fmt::Write for Prefix {
        fn write_str(&mut self, value: &str) -> fmt::Result {
            let count = value.len().min(self.remaining.len());
            if value.as_bytes()[..count] != self.remaining[..count] {
                return Err(fmt::Error);
            }
            self.remaining = &self.remaining[count..];
            self.matched = self.remaining.is_empty();
            if self.matched {
                Err(fmt::Error)
            } else {
                Ok(())
            }
        }
    }
    // Compare only fixed literals, retain no message data, and stop formatting
    // before the address/error suffix. Never forward an upstream event.
    let mut prefix = Prefix {
        remaining: expected.as_bytes(),
        matched: false,
    };
    let _ = fmt::write(&mut prefix, format_args!("{value:?}"));
    prefix.matched
}

#[cfg(test)]
#[path = "service_observation_client_tests.rs"]
mod tests;
