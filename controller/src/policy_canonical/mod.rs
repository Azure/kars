// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Per-kind canonical-form parsers + signing-pipeline trait.
//!
//! This module is the **single seam** between
//! [`crate::policy_fetcher`]'s kind-agnostic OCI+cosign verification
//! pipeline and the per-kind, byte-stable canonical-form rules defined
//! in `docs/internal/policy-canonical-format.md`.
//!
//! Adding a new policy kind (tools / inference / memory / mcp-tools /
//! eval-corpus) is three steps:
//!
//! 1. Create a sibling module `policy_canonical::<kind>` that exposes a
//!    `parse` function returning the kind's verified-output struct.
//! 2. Define a unit struct (e.g. `pub struct ToolsKind;`) and implement
//!    [`PolicyKind`] on it, with `Output` set to that verified struct.
//! 3. Call [`crate::policy_fetcher::fetch_and_verify_generic::<MyKind>`]
//!    from the kind's reconciler. The cosign trust-root, ACR auth,
//!    `validate_ref_shape`, cache, and `FetchError` mapping are all
//!    inherited — only the canonical parser is per-kind.
//!
//! Slice 1c.1 ships the trait + the [`EgressKind`] implementation (the
//! existing egress path, refactored). Slices 1c.2 - 1c.5 add the other
//! four kinds. The egress canonical bytes are **frozen** at v1; the
//! `EgressKind` parser MUST emit byte-identical results to the
//! pre-1c.1 inline `policy_fetcher::canonical::parse` it replaced.

use crate::policy_fetcher::FetchError;
use std::time::{Instant, SystemTime};

pub mod egress;
pub mod inference;
pub mod memory;
pub mod tools;

#[allow(unused_imports)]
// re-exported for downstream sub-slices (1c.4+); tests use the full path
pub use egress::EgressKind;
#[allow(unused_imports)]
pub use inference::InferenceKind;
#[allow(unused_imports)]
pub use memory::MemoryKind;
#[allow(unused_imports)]
pub use tools::ToolsKind;

/// One slot per `PolicyKind` for the per-kind verified-artifact cache.
///
/// The kind-agnostic core in [`crate::policy_fetcher`] uses this enum to
/// route cache reads + writes through the kind's static
/// `OnceLock<Mutex<HashMap<…>>>` without paying for `Box<dyn Any>`
/// downcasts on the hot path. Each kind adds one variant; the variant
/// payload is the kind's `Output` type.
///
/// We keep this enum private to the `policy_canonical` API surface — it
/// is the implementation detail of how
/// [`PolicyKind::cache_get`] / [`PolicyKind::cache_put`] are wired, not
/// a public extension point.
#[allow(dead_code)] // future kinds will add variants in 1c.5
pub(crate) enum CachedValue {
    Egress(egress::VerifiedAllowlist),
    Inference(inference::VerifiedInferencePolicy),
    Memory(memory::VerifiedMemoryBinding),
    Tools(tools::VerifiedAgtProfile),
}

/// Trait implemented by each policy kind (egress, tools, inference,
/// memory, mcp-tools, eval-corpus). Provides the per-kind constants
/// (OCI media type, canonical apiVersion + kind) and the canonical
/// parser. The signing pipeline + cache + ACR auth in
/// [`crate::policy_fetcher`] are generic over this trait.
///
/// ## Invariants every implementor MUST honor
///
/// 1. **Byte-stability.** `parse(bytes)` returns `Ok` *if and only if*
///    `bytes` exactly matches the canonical form defined for this kind
///    in `docs/internal/policy-canonical-format.md`. A producer that
///    signs structurally-valid-but-non-canonical bytes MUST be rejected
///    (otherwise the OCI digest cannot uniquely identify the policy
///    semantics).
/// 2. **`Output` shape forward-compat.** New fields added in a future
///    artifact version (`v2+yaml`) MUST land as new `Output` types
///    behind new media-type constants. A v1 implementor MUST refuse
///    v2 bytes (and vice versa). The MEDIA_TYPE constant is the
///    discriminator.
/// 3. **`finalize` is a pure stamp.** `parse` returns an `Output` with
///    `digest = ""` and `fetched_at = UNIX_EPOCH`; `finalize` writes
///    the verified `OciArtifactRef.digest` and the now-time. No other
///    field may be mutated by `finalize`.
/// 4. **Cache is per-kind.** Two kinds MUST NOT share a cache slot.
///    Even if two kinds happened to be byte-identical, their `Output`
///    types differ, and a hit-by-key from the wrong kind would mis-
///    type the cached value.
pub trait PolicyKind: 'static {
    /// OCI artifactType discriminator. The pulled `artifactType` MUST
    /// match this exactly; consumers reject any other value (forward-
    /// compat: v2 bumps the suffix; v1 consumers MUST refuse v2
    /// artifacts — see canonical-format doc §"Forward compatibility").
    const MEDIA_TYPE: &'static str;

    /// Pinned canonical `apiVersion` (e.g. `azureclaw.dev/v1alpha1`).
    #[allow(dead_code)] // used by per-kind tests + future sub-slices
    const API_VERSION: &'static str;

    /// Pinned canonical `kind` (e.g. `EgressAllowlist`, `ToolPolicy`).
    const KIND: &'static str;

    /// Verified-output struct returned to the reconciler. Must be
    /// `Clone + Send + Sync + 'static` so it can live in the
    /// process-wide cache and be returned across reconcile `await`
    /// boundaries.
    type Output: Clone + Send + Sync + 'static;

    /// Parse + canonical-form re-validate the artifact bytes. Called
    /// **after** cosign signature verification has already accepted
    /// the artifact; this catches the case where a producer signed
    /// structurally-valid-but-non-canonical bytes.
    ///
    /// On success, `Output.digest` is empty and `Output.fetched_at` is
    /// `UNIX_EPOCH` — the caller fills both via [`Self::finalize`]
    /// before returning to the reconciler.
    fn parse(bytes: &[u8]) -> Result<Self::Output, FetchError>;

    /// Stamp the verified `OciArtifactRef.digest` and now-time on the
    /// parsed output. See trait invariant #3.
    fn finalize(out: &mut Self::Output, digest: String, fetched_at: SystemTime);

    /// Per-kind cache: TTL-bounded lookup. Returns `None` on miss or
    /// when the entry exceeds [`CACHE_TTL`](crate::policy_fetcher::CACHE_TTL).
    fn cache_get(key: &str, now: Instant) -> Option<Self::Output>;

    /// Per-kind cache: insert. Existing entries with the same key are
    /// replaced (last-write-wins).
    fn cache_put(key: String, value: Self::Output);

    /// Per-kind cache: clear all entries. Test-only.
    #[cfg(test)]
    fn cache_clear();
}
