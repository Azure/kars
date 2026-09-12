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
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
    task::{Context, Poll},
};
use tower::{Layer, Service};
use tracing::{
    Subscriber,
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
    let port = std::env::var("KUBERNETES_SERVICE_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok());
    if host.as_deref() == config.cluster_url.host() && port == config.cluster_url.port_u16() {
        bits |= ENVIRONMENT;
    }
    let layer = ClientLayer {
        bits,
        namespace: config.default_namespace.clone(),
    };
    Ok(ClientBuilder::try_from(config)?.with_layer(&layer).build())
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
// discards every span field/event, including URLs and upstream error bodies.
struct HttpBoundary(Progress);

impl Subscriber for HttpBoundary {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.is_span()
            && metadata.name() == "HTTP"
            && metadata.target() == "kube_client::client::builder"
    }
    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        if self.enabled(attributes.metadata()) {
            self.0.set(DISPATCH);
        }
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, _: &tracing::Event<'_>) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[cfg(test)]
#[path = "service_observation_client_tests.rs"]
mod tests;
