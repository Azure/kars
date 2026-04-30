// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Prometheus exporter for the A2A gateway.
//!
//! Single registry; metric names are stable across releases (they
//! feed dashboards / SLO alerts in `docs/operations/a2a-gateway.md`).

use prometheus::{Encoder, IntCounterVec, IntGauge, Registry, TextEncoder};
use std::sync::OnceLock;

pub struct Metrics {
    pub registry: Registry,
    pub requests_total: IntCounterVec,
    pub rejections_total: IntCounterVec,
    pub active_connections: IntGauge,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();
        let requests_total = IntCounterVec::new(
            prometheus::Opts::new(
                "a2a_gateway_requests_total",
                "Total inbound A2A requests, by verified subject and outcome",
            ),
            &["subject", "outcome"],
        )
        .expect("requests_total opts");
        let rejections_total = IntCounterVec::new(
            prometheus::Opts::new(
                "a2a_gateway_rejections_total",
                "Total rejections, by reason (jws_invalid, unknown_issuer, replay, rate_limited)",
            ),
            &["reason"],
        )
        .expect("rejections_total opts");
        let active_connections = IntGauge::new(
            "a2a_gateway_active_connections",
            "Active TLS connections to the public listener",
        )
        .expect("active_connections opts");

        registry
            .register(Box::new(requests_total.clone()))
            .expect("register requests_total");
        registry
            .register(Box::new(rejections_total.clone()))
            .expect("register rejections_total");
        registry
            .register(Box::new(active_connections.clone()))
            .expect("register active_connections");

        Self {
            registry,
            requests_total,
            rejections_total,
            active_connections,
        }
    }

    pub fn render(&self) -> String {
        let metric_families = self.registry.gather();
        let encoder = TextEncoder::new();
        let mut buffer = Vec::new();
        encoder
            .encode(&metric_families, &mut buffer)
            .expect("encode metrics");
        String::from_utf8(buffer).expect("metrics utf8")
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL: OnceLock<Metrics> = OnceLock::new();

pub fn global() -> &'static Metrics {
    GLOBAL.get_or_init(Metrics::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_render_includes_known_series() {
        let m = Metrics::new();
        m.requests_total.with_label_values(&["alice", "ok"]).inc();
        m.rejections_total.with_label_values(&["jws_invalid"]).inc();
        m.active_connections.set(7);
        let out = m.render();
        assert!(out.contains("a2a_gateway_requests_total"));
        assert!(out.contains("a2a_gateway_rejections_total"));
        assert!(out.contains("a2a_gateway_active_connections 7"));
        assert!(out.contains(r#"reason="jws_invalid""#));
    }

    #[test]
    fn global_returns_same_instance() {
        let a = global() as *const _;
        let b = global() as *const _;
        assert_eq!(a, b);
    }
}
