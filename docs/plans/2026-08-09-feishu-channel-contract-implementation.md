# Feishu Channel Contract Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add typed Feishu channel policy and per-sandbox credentials with first-class OpenClaw and Hermes WebSocket adapters, while rejecting unsupported runtimes.

**Architecture:** `KarsSandbox.spec.channels[]` is the source of truth for non-sensitive channel policy; a dedicated immutable, versioned Secret selected by `credentialSecretRef` holds App ID/App Secret. The controller validates runtime capability, App ownership, and Secret identity, then injects policy plus explicit credential key refs only into the runtime container. OpenClaw and Hermes entrypoints translate the common contract into their native configuration, while image builds pin their runtime-specific Feishu dependencies.

**Tech Stack:** Rust 2024/kube-rs/schemars, Kubernetes CRD CEL/Secret/env, TypeScript Commander/Vitest, Bash entrypoints, OpenClaw 2026.5.27, Hermes Agent 0.16.0, Helm.

---

### Task 1: Typed channel CRD and capability validation

**Files:**
- Modify: `controller/src/crd.rs`
- Modify: `controller/src/crd_validations.rs`
- Modify: `controller/src/reconciler/runtime.rs`
- Modify: `deploy/helm/kars/templates/crd.yaml`
- Test: `controller/src/crd.rs`
- Test: `controller/src/reconciler/runtime.rs`
- Test: `controller/src/helm_drift.rs`

1. Write failing serialization/default tests for `spec.channels[].type=Feishu` and safe policy defaults.
2. Write failing runtime capability tests accepting OpenClaw/Hermes and rejecting all other runtime kinds.
3. Implement typed Rust structs/enums and defensive capability validation.
4. Add matching Helm schema/CEL and Rust-generated CRD validation parity.
5. Run focused CRD/runtime/drift tests.

### Task 2: Controller policy translation and secret isolation

**Files:**
- Modify: `controller/src/reconciler/mod.rs`
- Modify: `controller/src/reconciler/tests.rs`
- Modify: `controller/src/status/conditions.rs`
- Modify: `controller/src/status/mod.rs`

1. Write failing tests for Feishu policy env generation and runtime-container-only injection.
2. Write failing tests for unsupported runtime and missing/partial credential status outcomes.
3. Implement policy env compilation (`FEISHU_DOMAIN`, WebSocket mode, DM/group policy, ID lists, mention requirement).
4. Validate the credential Secret shape without copying secret values into status/logs.
5. Add `ChannelReady` vocabulary and fail-closed status conditions.
6. Verify router/init containers do not receive Feishu credentials or policy env.

### Task 3: CLI create and credential rotation

**Files:**
- Modify: `cli/src/commands/add.ts`
- Modify: `cli/src/commands/add.test.ts`
- Modify: `cli/src/commands/credentials.ts`
- Modify: `cli/src/config.ts`
- Test: `cli/src/commands/add.test.ts`
- Test: `cli/src/config.test.ts`

1. Write failing tests for Feishu flags, CR policy output, Secret mapping, and unsupported runtimes.
2. Add App ID/App Secret and policy flags to `kars add`.
3. Keep App credentials out of KarsSandbox/dry-run output and put them in a dedicated immutable Feishu Secret.
4. Extend local secret lookup and `kars credentials update` rotation with staged/adopted Secret revisions and a `resourceVersion`-guarded JSON Patch that changes only `credentialSecretRef`.
5. Run add/config/credentials tests, typecheck, and build.

### Task 4: OpenClaw image and adapter

**Files:**
- Modify: `sandbox-images/openclaw/Dockerfile.base`
- Modify: `sandbox-images/openclaw/Dockerfile`
- Modify: `sandbox-images/openclaw/entrypoint.sh`
- Create: `sandbox-images/openclaw/testM_feishu_channel.sh`

1. Write a failing shell test for complete credentials, partial credentials, policy translation, and plugin enablement.
2. Install `@openclaw/feishu` at the exact OpenClaw version during image build and make discovery fail closed.
3. Preserve the Feishu plugin in the pruned runtime plugin layout or install it in the immutable external plugin directory.
4. Generate `channels.feishu`, `plugins.allow`, and `plugins.entries` from the common env contract.
5. Fail loud on partial credentials or missing plugin.
6. Run shell syntax and behavior tests.

### Task 5: Hermes image and adapter

**Files:**
- Modify: `sandbox-images/hermes/Dockerfile`
- Modify: `sandbox-images/hermes/entrypoint.sh`
- Create: `sandbox-images/hermes/testM_feishu_channel.sh`

1. Write a failing shell/image-shape test for Feishu dependencies and env/config translation.
2. Install the pinned `hermes-agent[feishu]==0.16.0` dependency set.
3. Translate common credentials and WebSocket/domain policy to Hermes native env/config.
4. Map or enforce DM Pairing, group Allowlist, allowed IDs, and mention gating without weakening policy.
5. Fail loud on partial credentials or dependency import failure.
6. Run shell syntax, behavior, and Hermes runtime tests.

### Task 6: Channel readiness and App ownership

**Files:**
- Modify: `controller/src/reconciler/mod.rs`
- Modify: `controller/src/reconciler/tests.rs`
- Modify: `controller/src/status/conditions.rs`
- Modify: `controller/src/status/mod.rs`

1. Write failing tests for `ChannelReady` Configured/Connecting/Failed/Suspended and App fingerprint conflicts.
2. Implement a non-secret App ID fingerprint ownership record and finalizer cleanup, or explicitly narrow v1 to warning-only if cluster-wide ownership cannot be made race-free.
3. Consume a non-sensitive runtime readiness signal instead of treating Secret existence as connection success.
4. Gate overall `Ready=True` on declared channel readiness.
5. Verify credentials and message content never appear in status/events/metrics.

### Task 7: Documentation and regression validation

**Files:**
- Modify: `docs/channels-plugins.md`
- Modify: `docs/cli-reference.md`
- Modify: `docs/runtimes.md`
- Modify: `docs/hermes-plugin.md`
- Modify: `docs/security.md`
- Modify: `docs/api/crd-reference.md`
- Modify: `docs/api/conditions.md`
- Modify: `docs/operations/image-versioning.md`

1. Document Feishu Open Platform setup, WebSocket mode, permissions, IDs, pairing, group allowlist, egress, persistence, and rotation.
2. Document runtime capability differences and unsupported combinations.
3. Run Controller full tests, Clippy, Rust formatting, CLI tests/typecheck/lint/build, both shell tests, Dockerfile/image-shape checks, CRD server dry-run, and diff validation.
4. Request independent code review focused on credential leakage, policy weakening, plugin discovery, runtime capability, and ChannelReady truthfulness.
