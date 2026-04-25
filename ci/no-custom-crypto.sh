#!/usr/bin/env bash
# ci/no-custom-crypto.sh — enforces docs/implementation-plan.md §0.2 principle #8.
#
# No hand-rolled Signal/X3DH/Double-Ratchet, no hand-rolled OAuth/JWT signing,
# no hand-rolled Merkle chaining, no hand-rolled HMAC/KDF, no hand-rolled base64,
# no manual nonce/counter/IV construction — outside the explicit allowlist of
# files whose job is to wrap crypto in a SigningProvider / MeshProvider.
#
# Scope: production code only, diff-only (PR additions).
set -euo pipefail

BASE_REF="${BASE_REF:-origin/main}"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

ALLOW_PATHS=(
  'controller/src/providers/signing.rs'
  'controller/src/providers/mesh.rs'
  'controller/src/mesh_peer/'  # in-tree controller-side mesh peer hashing/signing — uses ed25519-dalek::SigningKey + Sha256 only; tracked for SigningProvider extraction in plan §4.1
  'inference-router/src/providers/signing.rs'
  'inference-router/src/providers/mesh.rs'
  'inference-router/src/a2a/agent_card.rs'      # AgentCard data model — references ed25519-dalek::VerifyingKey only for trust-anchor types; no signing primitives
  'inference-router/src/a2a/agent_projection.rs' # CRD → trust-anchor projection — re-exports ed25519-dalek::VerifyingKey only; verification-side surface
  'inference-router/src/a2a/card_server.rs'     # /.well-known/agent.json builder — wires SigningKey into card_signing::sign_card; no in-place crypto math
  'inference-router/src/a2a/card_signing.rs'    # RFC 7515 JWS / RFC 8037 EdDSA over AgentCards (A2A 1.0.0 §4.4.7) — standard JOSE primitive
  'inference-router/src/a2a/card_verifier.rs'   # inbound caller-card verifier — uses ed25519-dalek::VerifyingKey only (no signing primitives)
  'inference-router/src/a2a/jsonrpc_dispatch.rs' # JSON-RPC 2.0 binding for message/send / tasks/* — no crypto math; references ed25519-dalek types only via AP2 trust glue
  'inference-router/src/a2a/mandate_signing.rs' # AP2 IntentMandate / CartMandate / PaymentMandate Ed25519 sign — RFC 8032 EdDSA via ed25519-dalek; signs only, no key derivation
  'inference-router/src/a2a/mandate_trust_store.rs' # AP2 mandate verifier-side public-key store; uses ed25519-dalek::VerifyingKey only (no signing primitives)
  'inference-router/src/a2a/message_send_ap2.rs' # message/send AP2 glue — no in-line crypto math; consults mandate_signing/trust_store via traits
  'inference-router/src/a2a/snapshot_rebuild.rs' # trust-store snapshot rebuild — re-exports verification keys only
  'inference-router/src/a2a/trust_store.rs'  # A2A peer-card public-key store; uses ed25519-dalek::VerifyingKey only (verification side, no signing primitives)
  'inference-router/src/routes/a2a.rs'       # A2A axum routes — wires a2a::card_signing/mandate_signing into HTTP handlers; only imports ed25519-dalek::SigningKey for state plumbing
  'inference-router/src/auth.rs'          # IMDS/JWT verification; pre-existing
  'inference-router/src/handoff/mod.rs'   # pre-existing handoff AES-GCM blob cipher; plan §4.1 slates extraction into a SigningProvider-backed submodule
  'inference-router/src/handoff/crypto.rs' # extracted crypto submodule (AES-256-GCM + HKDF-SHA256 + integrity hash); single allow-listed home for the handoff blob cipher
  'inference-router/src/handoff/token.rs' # HandoffTokenStore — 32-byte random + SHA-256 hash + constant-time compare, extracted from mod.rs
  'vendor/'
  'tests/'
)

# Production paths to scan.
PROD_PATHS=(
  'controller/src/'
  'inference-router/src/'
  'cli/src/'
  'sandbox-images/'
  'policy-engine/'
)

# Patterns — each is a canonical import / invocation we never want written by us.
# Tuned for both Rust and TS.
RUST_PATTERNS='^\+use (sha2|hmac|curve25519_dalek|ed25519_dalek|x25519_dalek|aes|chacha20poly1305)::|^\+use ring::(signature|aead)::|^\+use x3dh::|^\+use double_ratchet::|^\+use signal[_-]protocol'
TS_PATTERNS="^\\+.*(from '@noble/curves'|from '@noble/hashes'|from 'tweetnacl'|from 'libsodium-wrappers'|require\\(['\"]crypto['\"]\\)\\.createHmac|require\\(['\"]crypto['\"]\\)\\.createSign|crypto\\.subtle\\.sign\\()"
MANUAL_PATTERNS='nonce *= *\[0u8|manual IV|manual nonce|Buffer\.from\(atob\(|= *base64\.decode\('

mapfile -t changed < <(
  git diff --name-only "${BASE_REF}...HEAD" 2>/dev/null || git diff --name-only HEAD
)

fail=0
for f in "${changed[@]}"; do
  [ -z "$f" ] && continue
  # skip allowlisted paths
  skip=0
  for a in "${ALLOW_PATHS[@]}"; do
    case "$f" in "$a"*) skip=1; break;; esac
  done
  [ "$skip" -eq 1 ] && continue
  # only scan prod paths
  match=0
  for prefix in "${PROD_PATHS[@]}"; do
    case "$f" in "$prefix"*) match=1; break;; esac
  done
  [ "$match" -eq 0 ] && continue
  [ -f "$f" ] || continue

  diff_added=$(git diff "${BASE_REF}...HEAD" -- "$f" 2>/dev/null | grep -E '^\+[^+]' || true)
  [ -z "$diff_added" ] && continue

  case "$f" in
    *.rs)
      hits=$(printf '%s\n' "$diff_added" | grep -E "$RUST_PATTERNS" || true)
      ;;
    *.ts|*.tsx|*.js|*.mjs)
      hits=$(printf '%s\n' "$diff_added" | grep -E "$TS_PATTERNS" || true)
      ;;
    *)
      hits=""
      ;;
  esac
  manual_hits=$(printf '%s\n' "$diff_added" | grep -E "$MANUAL_PATTERNS" || true)

  if [ -n "$hits" ] || [ -n "$manual_hits" ]; then
    echo "fail: $f introduces custom crypto. Route through providers/signing.rs, providers/mesh.rs, the AGT SDK, or 'ring'/'libsodium' via an allowlisted wrapper." >&2
    [ -n "$hits" ] && printf '  %s\n' "$hits" >&2
    [ -n "$manual_hits" ] && printf '  %s\n' "$manual_hits" >&2
    fail=1
  fi
done

exit $fail
