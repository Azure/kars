//! Provider contracts for the AzureClaw controller.
//!
//! Mirrors `inference-router/src/providers/` but scoped to controller-side
//! concerns: the controller chooses and wires providers based on
//! `ClawSandbox.spec.agt.providers`, and passes selected references through
//! to the router via ConfigMap.
//!
//! **Phase 0 status:** contracts only. No implementations and no
//! reconciler migrations land here. Provider construction and wiring are
//! Phase 1 scope per `docs/implementation-plan.md` §7.
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

// Scaffolding for Phase 1 — see docs/implementation-plan.md §7. Dead-code
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

/// Provider selection as read from `ClawSandbox.spec.agt.providers`.
/// See `docs/implementation-plan.md` §1.4.
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
        assert_eq!(ProviderKind::from_spec("vendored"), Some(ProviderKind::Vendored));
        assert_eq!(ProviderKind::from_spec("agt"), Some(ProviderKind::Agt));
        assert_eq!(ProviderKind::from_spec("null"), Some(ProviderKind::Null));
        assert_eq!(ProviderKind::from_spec("noop"), Some(ProviderKind::Null));
        assert_eq!(ProviderKind::from_spec("disabled"), Some(ProviderKind::Null));
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
        assert_eq!(field_managers::RECONCILER, "azureclaw-controller/reconciler");
        assert_eq!(field_managers::MESH, "azureclaw-controller/mesh");
        assert_eq!(field_managers::PAIRING, "azureclaw-controller/pairing");
        assert_eq!(
            field_managers::PROVIDER_BRIDGE,
            "azureclaw-controller/provider-bridge"
        );
    }
}
