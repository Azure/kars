// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared watch configuration for every CRD `Controller`.
//!
//! ## The bug this fixes (issue #451)
//!
//! Every reconciler used `watcher::Config::default()`. That is **not** "no watch
//! timeout" — kube-rs always sends `timeoutSeconds=290` to the API server
//! (`WatchParams::populate_qp` defaults to 290 when `Config.timeout` is `None`).
//! The problem is the **value**: on AKS the controller-to-API-server path
//! traverses a load balancer / konnectivity tunnel with an idle timeout (Azure
//! Standard LB default: **4 minutes / 240s**). A watch with no events sends no
//! packets, so the LB **silently drops** the idle TCP connection (no FIN/RST) at
//! 240s — *before* the server's 290s close can be delivered. kube-rs 3.1.0 then
//! polls the dead stream with a bare `stream.next().await` (no client-side idle
//! deadline), which hangs **forever**. The reflector store freezes while the 15s
//! requeue keeps reconciling the stale cached objects, so CR spec edits never
//! reach the compiled ConfigMap until a controller restart forces a fresh `List`.
//!
//! (kube-rs 4.0 adds a client-side `next_with_idle_timeout` backstop that caps
//! the freeze at ~295s instead of forever, but still ships **no HTTP/2 keepalive
//! ping** — so the fix below remains valuable there too.)
//!
//! ## The fix
//!
//! Set the watch `timeout` **below** the environment's idle window. The API
//! server then closes each watch every `timeout` seconds; that close is *traffic*
//! on the connection, so it can never sit idle long enough to be dropped, and the
//! watcher re-`List`/re-`watch`es (refreshing the store) within a bounded window.
//! This *prevents* the silent drop, which is why the value must be
//! `< idle_timeout`: a value **above** it (e.g. the 290s default vs Azure's 240s)
//! does not help.
//!
//! Default: **200s** (comfortably under Azure's 240s LB default; kube-rs also
//! rejects `timeout >= 295`). Operators on a different idle timeout can override
//! via `KARS_WATCH_TIMEOUT_SECS`.
//!
//! This is the version-independent, low-risk primary fix. The architecturally
//! superior, client-go-parity defense (HTTP/2 keepalive ping on the kube client,
//! ~45s dead-connection detection) is tracked as a follow-up — it needs a custom
//! hyper client stack and additional dependencies.

use kube::runtime::watcher;

/// Env var to override the watch timeout (seconds). Must be below the
/// environment's LB / proxy idle timeout for the silent-drop prevention to work.
pub const WATCH_TIMEOUT_ENV: &str = "KARS_WATCH_TIMEOUT_SECS";

/// Default watch timeout in seconds — under Azure Standard LB's 240s idle
/// default, with headroom for jitter.
pub const DEFAULT_WATCH_TIMEOUT_SECS: u32 = 200;

/// Resolve the configured watch timeout, honoring `KARS_WATCH_TIMEOUT_SECS`.
/// Values are clamped to a sane range (30s..=600s) so a typo can't disable the
/// protection or hammer the API server.
pub fn watch_timeout_secs() -> u32 {
    std::env::var(WATCH_TIMEOUT_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .map(|v| v.clamp(30, 600))
        .unwrap_or(DEFAULT_WATCH_TIMEOUT_SECS)
}

/// A `watcher::Config` with a bounded, idle-drop-safe timeout. Use this for
/// **every** primary and secondary (`.owns()` / `.watches()`) watch instead of
/// `watcher::Config::default()` so a silently-dropped watch can never freeze the
/// reflector store (issue #451).
pub fn bounded() -> watcher::Config {
    watcher::Config::default().timeout(watch_timeout_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_under_azure_lb_idle_window() {
        // The whole mechanism depends on the timeout firing BEFORE the 240s
        // Azure LB idle drop. Guard the invariant.
        const _: () = assert!(
            DEFAULT_WATCH_TIMEOUT_SECS < 240,
            "watch timeout must be below the 240s Azure LB idle default",
        );
        const _FLOOR: () = assert!(DEFAULT_WATCH_TIMEOUT_SECS >= 30);
    }

    #[test]
    fn bounded_config_sets_the_timeout() {
        let cfg = bounded();
        assert_eq!(cfg.timeout, Some(watch_timeout_secs()));
        assert!(cfg.timeout.is_some(), "bounded() must set a watch timeout");
    }

    #[test]
    fn env_override_is_clamped() {
        // Below floor → clamped to 30.
        unsafe { std::env::set_var(WATCH_TIMEOUT_ENV, "1") };
        assert_eq!(watch_timeout_secs(), 30);
        // Above ceiling → clamped to 600.
        unsafe { std::env::set_var(WATCH_TIMEOUT_ENV, "99999") };
        assert_eq!(watch_timeout_secs(), 600);
        // In range → honored.
        unsafe { std::env::set_var(WATCH_TIMEOUT_ENV, "150") };
        assert_eq!(watch_timeout_secs(), 150);
        // Garbage → default.
        unsafe { std::env::set_var(WATCH_TIMEOUT_ENV, "not-a-number") };
        assert_eq!(watch_timeout_secs(), DEFAULT_WATCH_TIMEOUT_SECS);
        unsafe { std::env::remove_var(WATCH_TIMEOUT_ENV) };
    }
}
