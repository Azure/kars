# Persistent Workspace Implementation Plan

**Status:** Implemented on `feature/persistent-workspace`.

Implementation notes:

- Storage/bootstrap pure helpers remain in `controller/src/reconciler/mod.rs`
	with focused tests in `controller/src/reconciler/tests.rs`; a separate
	`workspace_storage.rs` module was not introduced because the helpers share
	the reconciler's status and finalizer vocabulary.
- The bootstrap implementation is the image-baked
	`sandbox-images/openclaw/workspace-bootstrap.sh`, tested by executing it
	against temporary filesystems (including file and directory symlink attacks).
- Cluster-facing validation uses controller tests, Helm/Rust schema parity,
	live API-server CRD dry-run, and generated-manifest checks. A destructive
	persistence lifecycle E2E was not added to `tests/e2e/run.sh`; that suite
	requires a CSI provisioner and is tracked as follow-up coverage.

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add opt-in per-sandbox persistent `/sandbox` storage and declarative OpenClaw workspace bootstrap files while preserving the existing `emptyDir` default.

**Architecture:** Typed CRD fields describe dynamic or existing workspace claims and an optional same-namespace bootstrap ConfigMap. The Controller builds a pure workspace plan, reconciles storage before the Deployment, mounts the resulting volume only into the runtime container, and initializes approved Markdown files through a least-privilege init container. Storage readiness is surfaced independently and gates overall readiness.

**Tech Stack:** Rust 2024, kube-rs, k8s-openapi, schemars, Kubernetes PVC/ConfigMap/Deployment APIs, Helm CRD OpenAPI/CEL, shell entrypoint tests.

---

### Task 1: CRD storage and workspace schema

**Files:**
- Modify: `controller/src/crd.rs`
- Modify: `deploy/helm/kars/templates/crd.yaml`
- Test: `controller/src/crd.rs`

1. Add failing serialization/default tests for dynamic storage, existing claims and OpenClaw bootstrap configuration.
2. Add typed Rust schema with `Retain`, `ReadWriteOnce`, and `IfMissing` defaults.
3. Add matching Helm OpenAPI fields and CEL mutual-exclusion validation.
4. Run focused controller CRD tests and schema diagnostics.

### Task 2: Pure workspace resource planning

**Files:**
- Create: `controller/src/reconciler/workspace_storage.rs`
- Modify: `controller/src/reconciler/mod.rs`
- Test: `controller/src/reconciler/workspace_storage.rs`

1. Add failing tests for ephemeral, dynamically provisioned and existing-claim plans.
2. Implement a pure planner that returns the `sandbox-data` volume source and optional PVC object.
3. Verify Retain/Delete ownership metadata and immutable field validation.
4. Run focused planner tests.

### Task 3: PVC reconciliation and Pod mount

**Files:**
- Modify: `controller/src/reconciler/mod.rs`
- Modify: `controller/src/reconciler/tests.rs`

1. Add failing tests for the generated Pod volume and runtime-only mount.
2. Apply dynamic PVCs after namespace creation and before Deployment reconciliation.
3. Resolve existing claims and block Deployment readiness on missing/incompatible claims.
4. Replace only `sandbox-data.emptyDir` with `persistentVolumeClaim` when configured.
5. Verify existing pod-shape tests and new storage tests.

### Task 4: Bootstrap ConfigMap and init container

**Files:**
- Modify: `controller/src/reconciler/workspace_storage.rs`
- Modify: `controller/src/reconciler/mod.rs`
- Test: `controller/src/reconciler/workspace_storage.rs`

1. Add failing tests for allowed file validation, unsafe keys and init-container security shape.
2. Resolve the same-namespace ConfigMap before Deployment apply.
3. Add read-only bootstrap volume and a non-networked, non-privileged init container.
4. Implement `IfMissing` and `Always` atomic-copy behavior without logging contents.
5. Verify generated Pod JSON and security settings.

### Task 5: StorageReady status gating

**Files:**
- Modify: `controller/src/status/conditions.rs`
- Modify: `controller/src/status/mod.rs`
- Modify: `controller/src/reconciler/mod.rs`
- Test: `controller/src/status/mod.rs`

1. Add failing tests preventing `Ready=True` together with `StorageReady=False`.
2. Add StorageReady reasons and status construction.
3. Gate Deployment/Running status on claim and bootstrap readiness.
4. Verify status idempotency and suspended behavior.

### Task 6: OpenClaw default workspace preservation

**Files:**
- Modify: `sandbox-images/openclaw/entrypoint.sh`
- Create: `sandbox-images/openclaw/testM_workspace_defaults.sh`

1. Add a failing shell regression proving existing `AGENTS.md`, `SOUL.md`, and `TOOLS.md` survive initialization.
2. Extract create-if-missing helpers and preserve current first-run templates.
3. Verify shell syntax and the regression script.

### Task 7: Retain deletion semantics

**Files:**
- Modify: `controller/src/reconciler/mod.rs`
- Test: `controller/src/reconciler/workspace_storage.rs`
- Modify: `docs/plans/2026-08-07-persistent-workspace-design.md` only if the feasible namespace ownership model differs from the approved design.

1. Add a failing test that demonstrates namespace deletion destroys an in-namespace retained claim.
2. Implement a feasible retention model before claiming `Retain` support. Do not rely solely on owner-reference omission.
3. Ensure `Delete` remains explicit and destructive.
4. Verify CR deletion, namespace deletion warning, and explicit recovery behavior.

### Task 8: CLI, docs, and end-to-end validation

**Files:**
- Modify: `cli/src/commands/add.ts`
- Modify: `cli/src/commands/add.test.ts`
- Modify: `docs/api/crd-reference.md`
- Modify: `docs/api/lifecycle.md`
- Modify: `docs/runtimes/CONTRACT.md`
- Modify: `docs/security.md`
- Modify: `docs/cli-reference.md`
- Modify: `docs/getting-started.md`
- Modify: `tests/e2e/run.sh`

1. Add failing CLI tests for storage/bootstrap flags and destructive warnings.
2. Generate the new CR fields from CLI options.
3. Update public documentation and remove unconditional state-preservation claims.
4. Add lifecycle E2E coverage using a test PVC without bypassing the agent exec admission policy.
5. Run controller tests, CLI tests/typecheck/lint, shell checks and diff validation.
