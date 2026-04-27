//! Provider contracts for the AzureClaw controller.
//!
//! Mirrors `inference-router/src/providers/` but scoped to controller-side
//! concerns: the controller chooses and wires providers based on
//! `ClawSandbox.spec.agt.providers`, and passes selected references through
//! to the router via ConfigMap.
//!
//! **Phase 0 status:** contracts only. No implementations and no
//! reconciler migrations land here. Provider construction and wiring are
//! Phase 1 scope per internal Phase 1 plan §7.
//!
//! **Server-Side Apply** (plan §6 #4): each controller-side write that
//! touches provider-owned fields uses SSA with a stable field manager:
//!
//! - `azureclaw-controller/reconciler`  — base reconciler
//! - `azureclaw-controller/mesh`        — mesh provider ownership
//! - `azureclaw-controller/pairing`     — pairing reconciler
//! - `azureclaw-controller/provider-bridge` — provider-kind selection
//!
//! See [`field_managers`].

// Scaffolding for Phase 1 — see internal Phase 1 plan §7. Dead-code
// lints are silenced at the module level until call-sites land.
#![allow(dead_code)]

pub mod field_managers {
    //! Stable Server-Side Apply field managers per plan §6 #4.
    //!
    //! Every write that touches provider-owned fields carries one of these
    //! as `fieldManager`. The same manager is used across controller
    //! restarts and versions so conflict resolution converges.
    pub const RECONCILER: &str = "azureclaw-controller/reconciler";
    pub const MESH: &str = "azureclaw-controller/mesh";
    pub const PAIRING: &str = "azureclaw-controller/pairing";
    pub const PROVIDER_BRIDGE: &str = "azureclaw-controller/provider-bridge";
}

/// Selects which implementation of a contract a tenant uses.
/// See internal Phase 1 plan §1.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Vendored,
    Agt,
    /// Admission-policy-rejected in prod. Only accepted when the manifest
    /// carries `metadata.labels.azureclaw.azure.com/dev-only: "true"`.
    /// Static mirror: `ci/no-null-provider-prod.sh`.
    Null,
}

impl ProviderKind {
    /// Parse from the spec string. Returns `None` for unknown values so
    /// the controller can fail fast on unsupported input.
    pub fn from_spec(s: &str) -> Option<Self> {
        match s {
            "vendored" => Some(Self::Vendored),
            "agt" => Some(Self::Agt),
            "null" | "noop" | "disabled" => Some(Self::Null),
            _ => None,
        }
    }

    /// Is this choice allowed in production (no dev-only label)?
    pub fn allowed_in_prod(self) -> bool {
        !matches!(self, Self::Null)
    }
}

/// Outage mode selected per `ClawSandbox` via `spec.agt.outageMode`.
/// See internal Phase 1 plan §1.3.
///
/// The router-side enforcement, pure decision function, and serde wire
/// format live in `inference-router/src/providers/outage.rs`. The
/// controller repeats the enum here (with a parser + env-validation
/// helper) so reconciler admission rejects `degradedDev` on non-dev
/// sandboxes before the router ever sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutageMode {
    /// Fail-closed. Default per §0.2 #8.
    #[default]
    Strict,
    /// Allow when a cached decision is under TTL; else fail-closed.
    CachedRead,
    /// Fail-open with a warning label. Rejected in prod.
    DegradedDev,
}

impl OutageMode {
    /// Parse from the CRD wire value. Accepts camelCase, kebab-case and
    /// snake_case — matches the router-side parser so a sandbox's
    /// `spec.agt.outageMode` round-trips identically through both sides.
    pub fn from_spec(s: &str) -> Option<Self> {
        match s {
            "strict" | "Strict" => Some(Self::Strict),
            "cachedRead" | "cached-read" | "cached_read" => Some(Self::CachedRead),
            "degradedDev" | "degraded-dev" | "degraded_dev" => Some(Self::DegradedDev),
            _ => None,
        }
    }

    /// `true` only for `DegradedDev` — the sole mode admission must
    /// restrict to dev-only sandboxes.
    pub fn is_dev_only(self) -> bool {
        matches!(self, Self::DegradedDev)
    }

    /// Returns `Err` when the chosen mode is illegal for the environment.
    /// `is_dev_env` must be `true` only when the `ClawSandbox` already
    /// carries `metadata.labels.azureclaw.azure.com/dev-only=true` — that
    /// admission check lives in the null-provider VAP and must have run
    /// before this helper is consulted.
    pub fn validate_for_env(self, is_dev_env: bool) -> Result<(), OutageModeError> {
        if self.is_dev_only() && !is_dev_env {
            return Err(OutageModeError::DegradedDevInProd);
        }
        Ok(())
    }
}

/// Reasons an `OutageMode` value is rejected by the controller before it
/// ever reaches the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutageModeError {
    DegradedDevInProd,
}

impl std::fmt::Display for OutageModeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DegradedDevInProd => f.write_str(
                "outageMode=degradedDev requires the sandbox to carry metadata.labels.azureclaw.azure.com/dev-only=true",
            ),
        }
    }
}

impl std::error::Error for OutageModeError {}

/// The four provider kinds a `ClawSandbox` selects. Each field reads from
/// `spec.agt.providers.{mesh,policy,audit,signing}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSelection {
    pub mesh: ProviderKind,
    pub policy: ProviderKind,
    pub audit: ProviderKind,
    pub signing: ProviderKind,
}

impl Default for ProviderSelection {
    fn default() -> Self {
        // Phase 0 default: vendored across the board (zero behaviour change).
        Self {
            mesh: ProviderKind::Vendored,
            policy: ProviderKind::Vendored,
            audit: ProviderKind::Vendored,
            signing: ProviderKind::Vendored,
        }
    }
}

impl ProviderSelection {
    /// Returns `true` if any provider is `Null`. Admission-policy callers
    /// combine this with the dev-only label check; the same logic lives in
    /// `ci/no-null-provider-prod.sh` for static enforcement.
    pub fn has_null(&self) -> bool {
        self.mesh == ProviderKind::Null
            || self.policy == ProviderKind::Null
            || self.audit == ProviderKind::Null
            || self.signing == ProviderKind::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_kind_parses_all_aliases() {
        assert_eq!(
            ProviderKind::from_spec("vendored"),
            Some(ProviderKind::Vendored)
        );
        assert_eq!(ProviderKind::from_spec("agt"), Some(ProviderKind::Agt));
        assert_eq!(ProviderKind::from_spec("null"), Some(ProviderKind::Null));
        assert_eq!(ProviderKind::from_spec("noop"), Some(ProviderKind::Null));
        assert_eq!(
            ProviderKind::from_spec("disabled"),
            Some(ProviderKind::Null)
        );
        assert_eq!(ProviderKind::from_spec("vendoreed"), None);
        assert_eq!(ProviderKind::from_spec(""), None);
    }

    #[test]
    fn null_is_not_allowed_in_prod() {
        assert!(ProviderKind::Vendored.allowed_in_prod());
        assert!(ProviderKind::Agt.allowed_in_prod());
        assert!(!ProviderKind::Null.allowed_in_prod());
    }

    #[test]
    fn default_selection_is_vendored_across_the_board() {
        let sel = ProviderSelection::default();
        assert_eq!(sel.mesh, ProviderKind::Vendored);
        assert_eq!(sel.policy, ProviderKind::Vendored);
        assert_eq!(sel.audit, ProviderKind::Vendored);
        assert_eq!(sel.signing, ProviderKind::Vendored);
        assert!(!sel.has_null());
    }

    #[test]
    fn has_null_detects_any_null_field() {
        let sel = ProviderSelection {
            audit: ProviderKind::Null,
            ..ProviderSelection::default()
        };
        assert!(sel.has_null());
    }

    #[test]
    fn field_managers_are_stable_strings() {
        assert_eq!(
            field_managers::RECONCILER,
            "azureclaw-controller/reconciler"
        );
        assert_eq!(field_managers::MESH, "azureclaw-controller/mesh");
        assert_eq!(field_managers::PAIRING, "azureclaw-controller/pairing");
        assert_eq!(
            field_managers::PROVIDER_BRIDGE,
            "azureclaw-controller/provider-bridge"
        );
    }

    #[test]
    fn outage_mode_default_is_strict() {
        assert_eq!(OutageMode::default(), OutageMode::Strict);
    }

    #[test]
    fn outage_mode_parses_camel_kebab_snake() {
        assert_eq!(OutageMode::from_spec("strict"), Some(OutageMode::Strict));
        assert_eq!(
            OutageMode::from_spec("cachedRead"),
            Some(OutageMode::CachedRead)
        );
        assert_eq!(
            OutageMode::from_spec("cached-read"),
            Some(OutageMode::CachedRead)
        );
        assert_eq!(
            OutageMode::from_spec("cached_read"),
            Some(OutageMode::CachedRead)
        );
        assert_eq!(
            OutageMode::from_spec("degradedDev"),
            Some(OutageMode::DegradedDev)
        );
        assert_eq!(OutageMode::from_spec("nope"), None);
        assert_eq!(OutageMode::from_spec(""), None);
    }

    #[test]
    fn outage_mode_dev_only_flag() {
        assert!(!OutageMode::Strict.is_dev_only());
        assert!(!OutageMode::CachedRead.is_dev_only());
        assert!(OutageMode::DegradedDev.is_dev_only());
    }

    #[test]
    fn outage_mode_validate_rejects_degraded_dev_in_prod() {
        assert_eq!(
            OutageMode::DegradedDev.validate_for_env(false),
            Err(OutageModeError::DegradedDevInProd)
        );
        assert!(OutageMode::DegradedDev.validate_for_env(true).is_ok());
        // Non-dev-only modes are legal in both environments.
        assert!(OutageMode::Strict.validate_for_env(false).is_ok());
        assert!(OutageMode::Strict.validate_for_env(true).is_ok());
        assert!(OutageMode::CachedRead.validate_for_env(false).is_ok());
        assert!(OutageMode::CachedRead.validate_for_env(true).is_ok());
    }
}
