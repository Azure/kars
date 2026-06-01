# CI optimisation plan — kars

**Date**: 2026-06-01
**Current baseline**: 30 min wall-clock per CI run on `main`. **Critical path**: `rust-build` (26.5 min) + `bench-regression` (29.6 min). Everything else under 1 min thanks to existing path-detect skips.
**Target**: ≤10 min wall-clock for the common PR path (docs / CLI / TS only), ≤15 min for full Rust-touch path. Get bench off the critical path entirely.

---

## Current state — what we have today (the good)

| Job | Time | Notes |
|---|---|---|
| `rust-build` | 26.5 min | `Swatinem/rust-cache@v2` with `shared-key: rust-build`; runs fmt + clippy + build --release + test --all |
| `bench-regression` | 29.6 min | Compiles + runs **2** criterion benches; uses the same shared rust-cache |
| `cargo-audit` | 0.3 min | path-detect gates skip when no deps change |
| `cargo-deny` | 0.4 min | path-detect gates skip when no deps change |
| `cli-build` | 0.9 min | npm cache via `setup-node` |
| `runtime-openclaw-build` | 0.7 min | npm cache |
| `mesh-plugin-build` | 0.4 min | npm cache |
| `python-sidecar` | 0.2 min | **no-op** when `agt-sidecar/pyproject.toml` not present (it isn't) |
| `cosign-verify` | 0.2 min | **dry-run only** on PRs (records the command but never executes it) |
| `bicep-validate` | 0.4 min | `az bicep build` on one file |
| `helm-lint` | 0.2 min | `helm lint deploy/helm/kars` |
| `security-scan` (Trivy) | 0.7 min | fs scan, severity HIGH+CRITICAL, uploads SARIF |
| `container-scan` (Trivy) | 0.6 min | fs mode again, redundant with security-scan |
| `dockerfile-lint` (hadolint) | 0.2 min | lints all Dockerfiles |
| `chaos-tier` | 0.6 min | scoped to chaos crate only |
| `e2e` (Kind) | 5.5 min | path-gated; pre-builds 2 images via buildx GHA cache |

**The good news**: the architecture is already pretty mature. The path-detect gates work well — most PR paths skip 80% of jobs.

**The problem**: when `cargo.toml` or any Rust file changes, you eat the full 30 min because:
1. `rust-build` does fmt + clippy + build --release + test --all sequentially
2. `bench-regression` rebuilds nearly the same dep graph independently (different `--bench` targets force a fresh codegen pass; cache shared-key helps deps but not benches themselves)
3. `image-cache-publish.yml` exists to warm Docker GHA cache for `e2e`, but it doesn't help `rust-build` at all

---

## Concrete issues I found

### 🔴 HIGH — `bench-regression` is on the PR critical path (29.6 min)

The comment says it's a "permanent CI surface" but **the bench compile + 2 bench runs add 29 minutes to every PR**, including doc-only PRs that wouldn't otherwise touch Rust. The path-detect that gates other Rust jobs **isn't applied to bench-regression**.

### 🟡 MEDIUM — `rust-build` does 4 sequential cargo invocations

```
cargo fmt --all -- --check       # ~5s
cargo clippy --all-targets ...    # ~7-10 min (full build artifacts)
cargo build --release             # ~5-8 min (rebuilds in release profile)
cargo test --all                  # ~8-10 min (debug profile rebuild + run tests)
```

Three of these (clippy, build, test) build the workspace from scratch in different profiles. **clippy's artifacts are not reused by `cargo build --release`** (different `RUSTFLAGS` + opt levels). Same for `cargo test` (debug profile).

### 🟡 MEDIUM — `security-scan` and `container-scan` are both Trivy fs mode (redundant)

```
security-scan:  trivy fs . severity=CRITICAL,HIGH
container-scan: trivy fs . severity=HIGH,CRITICAL
```

Two near-identical Trivy fs scans, 0.7 + 0.6 min. Should be one job.

### 🟡 MEDIUM — `cosign-verify` is dry-run on PRs (no-op)

It installs cosign, prints the verification command, never executes. Should either:
- Verify against the latest published images (real check), OR
- Move to `release.yml` only (currently sits on every PR doing nothing)

### 🟡 MEDIUM — `python-sidecar` is a permanent no-op

The job checks `if [ -f agt-sidecar/pyproject.toml ]` — the file doesn't exist and never has. The job runs on every PR, takes 12s, and silently does nothing.

### 🟢 LOW — `image-cache-publish.yml` triggers on every push but only main/dev branch pushes warm the right cache

Path filter is correct; this is mostly fine. But there's no fallback if it falls behind (e.g., concurrent merges). The `e2e` job comments admit it "occasionally" needs to rebuild from scratch.

### 🟢 LOW — `bicep-validate` doesn't catch much

Only `az bicep build --file deploy/bicep/main.bicep`. Doesn't catch `--check-only` lint warnings; doesn't validate other bicep files. Cheap so not a perf concern but is a coverage gap.

### 🟢 LOW — `dependency-review.yml`, `secret-scanning.yml`, `dependency-review.yml` are all separate workflows with overlapping triggers

Not a perf issue (they run in parallel) but adds GHA orchestration overhead.

---

## Proposed optimisation plan — phased

### Phase 1 — pure-config wins (no code changes; estimated ~50% wall-clock reduction)

#### 1.1 Gate `bench-regression` on Rust-touching paths

Add a path filter that matches what `image-cache-publish.yml` and `e2e` already use:

```yaml
  bench-regression:
    name: Bench Regression (criterion)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 2 }
      - name: Detect Rust-affecting changes
        id: paths
        run: |
          if [ "${{ github.event_name }}" = "push" ]; then
            CHANGED=$(git diff --name-only HEAD~1 HEAD)
          else
            CHANGED=$(git diff --name-only origin/${{ github.base_ref }}...HEAD)
          fi
          if echo "$CHANGED" | grep -qE '^(controller|inference-router|kars-a2a-core|a2a-gateway|Cargo\.toml|Cargo\.lock)'; then
            echo "run=true" >> "$GITHUB_OUTPUT"
          else
            echo "run=false" >> "$GITHUB_OUTPUT"
            echo "::notice::No Rust paths changed — skipping bench-regression"
          fi
      # ... wrap every existing step with `if: steps.paths.outputs.run == 'true'`
```

**Saved**: ~29 min on every doc-only / CLI-only / TS-only PR (which is most of them).

#### 1.2 Run bench-regression in parallel with `rust-build`, not after

Already in parallel — both jobs depend only on `actions/checkout`. Confirmed. Skip.

#### 1.3 Drop `python-sidecar` job entirely (or guard at workflow level)

Add a `paths:` filter on the workflow trigger if you want to keep the job for future, OR delete it. The detection comment in the job says "AGT sidecar not yet present" — has been "not yet present" for the full life of this repo.

**Saved**: 0.2 min × every PR. Tiny but removes a maintenance no-op.

#### 1.4 Merge `security-scan` and `container-scan` into one Trivy job

Both run `trivy fs`. One job with `severity: CRITICAL,HIGH,MEDIUM` (broader coverage; same time).

**Saved**: 0.6 min + cleanup.

#### 1.5 Move `cosign-verify` from `ci.yml` to `release.yml` only (or actually run it on PRs)

Two options:
- **A (cheap)**: delete from `ci.yml`, add to `release.yml` post-publish to verify the freshly-signed images.
- **B (better)**: in `ci.yml`, verify the **previous release's** images (proves the keyless OIDC trust chain is intact). Real check, ~30s.

Recommend **B** — currently the job is a tutorial comment more than a check.

#### 1.6 Drop `cargo build --release` from `rust-build` (combine with `cargo test`)

`cargo test --release --all` builds the same artifacts you need for tests AND validates the release profile in one pass. Drops one full cycle of codegen.

**Saved**: 5–8 min off `rust-build`.

**Result of Phase 1**: PR critical path drops from ~30 min to **~15 min** for Rust changes, **~3 min** for non-Rust (the bench gate eats most of the saving).

---

### Phase 2 — Rust caching wins (modest code changes; ~30% more reduction)

#### 2.1 Add `sccache` as a Rust compiler cache backend

Layered on top of Swatinem/rust-cache (which caches `target/`), `sccache` caches individual compilation units across cargo invocations. The result is:
- `cargo clippy` and `cargo test` share cached compilation units
- Different feature combinations don't re-compile shared crates

```yaml
      - uses: mozilla-actions/sccache-action@v0.0.6
        with: { version: "v0.8.2" }
      - name: Configure sccache
        run: |
          echo "RUSTC_WRAPPER=sccache" >> "$GITHUB_ENV"
          echo "SCCACHE_GHA_ENABLED=true" >> "$GITHUB_ENV"
```

Add to all Rust-running jobs (rust-build, cargo-audit, cargo-deny, bench-regression, chaos-tier).

**Saved**: 30-50% off `cargo clippy + build + test` cycle when the cache hits. First run after a Cargo.lock change still pays full cost, but subsequent runs are fast.

#### 2.2 Add `cargo nextest` for faster test runs

`cargo nextest` runs tests in parallel with better isolation and 60% faster than `cargo test` in most workloads:

```yaml
      - uses: taiki-e/install-action@nextest
      - run: cargo nextest run --workspace --release
```

**Saved**: ~3-5 min off the test step.

#### 2.3 Split `rust-build` matrix into clippy + test (parallel)

Currently sequential: fmt → clippy → build → test. Split into 2 matrix jobs:
- `lint`: fmt + clippy
- `test`: nextest

They run in parallel; clippy doesn't block test; saves ~half of `rust-build`'s remaining time.

**Saved**: ~5-7 min critical path.

---

### Phase 3 — Docker build dedup (major; needs careful refactor)

#### 3.1 Replace `cargo build` inside Dockerfiles with `cargo chef` two-stage builds

Today: `image-cache-publish.yml` builds Docker images, and `rust-build` builds Rust binaries — **the same `cargo build --release` runs twice on every Rust-changing PR**. Once on the host (for tests + clippy) and again inside Docker (for image production).

**Fix**: use `cargo-chef` to split the Dockerfile into:
1. A `chef prepare` stage that creates a recipe.json of just the deps
2. A `chef cook --release` stage that builds *only* deps (cached forever unless Cargo.lock changes)
3. The final `cargo build --release` stage that compiles only first-party code

This makes Docker builds incremental even when the host build wasn't shared. Combined with buildx GHA cache (already in place), Docker builds drop from 8-12 min to 1-2 min for first-party-only changes.

**Saved**: ~10 min on every `image-cache-publish.yml` run and ~6 min on the `e2e` job's pre-build step.

#### 3.2 Build Rust binaries ONCE on the host, COPY them into Docker

The cleanest version of 3.1: do `cargo build --release` once on the runner (in `rust-build`), upload the binaries as artifacts, and have the Dockerfile just `COPY` them into a minimal base image.

Pros: zero duplicate cargo work; smallest possible Docker images.
Cons: must use `--target x86_64-unknown-linux-musl` (or distroless) to match the runtime, and the host runner must produce the same glibc as the target image.

This is the "best practice" pattern but a bigger refactor (each Dockerfile gets simpler; CI orchestration gets one more "build → upload → download → bake" step).

**Saved**: ~15 min total across rust-build + image-cache-publish (vs. ~30 min today).

---

### Phase 4 — Workflow consolidation (cleanup)

#### 4.1 Merge `secret-scanning.yml`, `dependency-review.yml`, `scorecard.yml` into one `security-meta.yml`

Three workflows, ~80 LOC total, all PR-triggered, all read-only. One workflow with three jobs is cleaner.

#### 4.2 Move `bench-regression` to its own workflow file

Currently inside `ci.yml`. Splitting it out lets you skip the whole workflow on doc-only changes via `paths-ignore: ['docs/**', '**/*.md']` at the workflow level — cheaper than per-job path detection.

#### 4.3 Use `needs:` to express the real DAG instead of fire-and-forget parallelism

Today every job runs in parallel. For correctness this is fine, but `e2e` could `needs: [rust-build]` to share the warm cache + avoid double-compile if the cache misses.

---

### Phase 5 — Runner upgrades (optional, $$$)

Current: `ubuntu-latest` (2-core, 7 GB RAM) for everything.

For the heavy `rust-build` and `bench-regression`, switching to **`ubuntu-latest-8-core` (or larger)** would cut wall-clock by 50-70% for parallel-friendly workloads (cargo build, cargo test). Costs about 4× the per-minute price but the runtime drops more than 4×.

GitHub-hosted larger runners are billed per minute; for kars's CI volume (a few PRs/day), the cost difference is ~$20/month. Likely worth it.

---

## Final priority-ordered action list

| # | Action | Effort | Saved (PR critical path) | Risk |
|---|---|---|---|---|
| 1 | Gate `bench-regression` on Rust paths | 15 min | **29 min on most PRs** | Low |
| 2 | Drop `cargo build --release` step (fold into nextest) | 5 min | 5–8 min | Low |
| 3 | Delete or guard `python-sidecar` job | 10 min | 0.2 min × every PR + cleanup | None |
| 4 | Merge `security-scan` + `container-scan` Trivy jobs | 15 min | 0.6 min + cleanup | None |
| 5 | Fix `cosign-verify` to actually verify previous-release images | 15 min | None (better coverage) | Low |
| 6 | Add `sccache` to all Rust jobs | 20 min | 30–50% off compile time | Low |
| 7 | Switch `cargo test` → `cargo nextest` | 15 min | 3–5 min off test step | Low |
| 8 | Split `rust-build` into `lint` + `test` parallel jobs | 20 min | 5–7 min critical path | Medium (review CI logs) |
| 9 | Adopt `cargo-chef` in Rust Dockerfiles | 2–4 hr | 10 min on `image-cache-publish.yml` | Medium |
| 10 | (Optional) Move to `ubuntu-latest-8-core` for Rust jobs | 5 min config + $$ | ~50% off compile time | Cost only |
| 11 | (Optional) Build binaries once + COPY into Docker | 1 day | 15 min total | Higher — needs glibc/musl decision |

## Recommended sequence

**Quick wins (1–2 hours total, ships in one PR)**: items 1–5
**Result**: doc-only PR ~3 min; CLI/TS PR ~5 min; Rust PR ~22 min

**Medium investment (1 day total)**: items 6–8
**Result**: Rust PR ~10–12 min

**Larger investment (1–2 days)**: item 9 (and optionally 11)
**Result**: Rust PR ~8 min; image rebuilds ~2 min

The numbers I'd target as "state of the art":
- doc-only PR: **<2 min**
- CLI/TS PR: **<4 min**
- Rust PR: **<10 min**

That's achievable with items 1–9. The current 30 min is fixable; nothing about kars requires the long path.

---

## Honest answer on your "running but doesn't check" concern

Yes, you're right. Two clear examples:

1. **`python-sidecar`**: runs on every PR, executes `if [ -f agt-sidecar/pyproject.toml ]` (file does not exist anywhere in repo history), prints `"AGT sidecar not yet present — skipping lint & test"`, and exits 0. This has been a no-op since day 1.
2. **`cosign-verify`**: installs cosign, prints the verification command as a heredoc, and exits 0. The comment says *"PRs run in dry-run mode because not every PR re-signs images"* — but the dry-run is literally just `cat <<EOF`. No verification happens.

Both are accidentally green ✅ on every run. Items 3 and 5 above fix them.

Anything else I'd flag as low-value but not a no-op:
- `dockerfile-lint` (hadolint) does lint, but uses default config — no kars-specific rules. Could add `.hadolint.yaml` for ignored rules. Minor.
- `bicep-validate` only validates the entry point `main.bicep`. Other bicep files (`agent-id-trust.bicep`) aren't linted.

---

## Addendum — How to make Rust build ONCE per CI run (user follow-up)

User asked: *"what will prevent us having to build the rust binaries this many times — the e2e test I believe also builds the rust..."*

Yes — on a Rust-touching PR, the same Rust source is compiled **5 times** in 3 different profiles:

| Build # | Where | Profile | Notes |
|---|---|---|---|
| 1 | `rust-build` job: `cargo clippy --all-targets` | debug+check | Swatinem cache |
| 2 | `rust-build` job: `cargo build --release` | release | Swatinem cache (partial — clippy's check artifacts ≠ build's codegen) |
| 3 | `rust-build` job: `cargo test --all` | test (debug) | Swatinem cache (debug profile fresh again) |
| 4 | `bench-regression` job: `cargo bench --no-run` | bench profile | Swatinem cache (yet another profile) |
| 5a | `e2e` job: Docker pre-build of controller image | release in container | buildx GHA cache (Dockerfile-layer granularity) |
| 5b | `e2e` job: Docker pre-build of router image | release in container | buildx GHA cache |
| (6) | `image-cache-publish` job (dev/main pushes): controller + router + a2a-gateway | release in container | buildx GHA cache |

### Why simple caching can't fix this

Cargo's `target/` is **profile-segregated**:

```
target/debug/        ← clippy + test artifacts (no codegen)
target/release/      ← cargo build --release
target/criterion/    ← bench artifacts (separate profile)
```

Even with a perfect Swatinem cache hit, each profile rebuilds independently. Docker buildx GHA cache is **Dockerfile-layer-level** — one source change → cache miss on the `RUN cargo build` layer → full rebuild.

### Three options, ordered by ambition

#### Option A — `cargo-chef` two-stage Dockerfiles (4 hr total)

Split each Rust Dockerfile into recipe + cook + build stages:
1. **Recipe**: extract `Cargo.toml + Cargo.lock` into `recipe.json` (no source)
2. **Cook**: build only deps from `recipe.json` (cached forever unless Cargo.lock changes)
3. **Build**: copy first-party source + build only the workspace crate

Result: changing `inference-router/src/routes/foo.rs` rebuilds only the inference-router crate (1–2 min), not 600+ transitive deps (8–10 min). Docker builds drop from 8–12 min to 1–2 min for first-party-only changes.

**Total CI per PR**: ~30 min → ~20 min. Still rebuilds inside Docker just much faster.

#### Option B — Build once on host, `COPY` binary into distroless Docker (½ day) ⭐ RECOMMENDED

The state-of-the-art Rust CI pattern. One `cargo build` per CI run, every other job consumes the binary artifact.

```yaml
jobs:
  build-rust:
    name: Build Rust binaries (musl static)
    runs-on: ubuntu-latest-8-core
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: x86_64-unknown-linux-musl }
      - uses: Swatinem/rust-cache@v2
        with: { shared-key: musl-release }
      - uses: mozilla-actions/sccache-action@v0.0.6
      - run: cargo build --release --target x86_64-unknown-linux-musl --workspace
      - uses: actions/upload-artifact@v4
        with:
          name: kars-binaries
          path: target/x86_64-unknown-linux-musl/release/kars-*
          retention-days: 1

  rust-test:
    needs: build-rust   # share the warm cache
    steps:
      - uses: Swatinem/rust-cache@v2
        with: { shared-key: musl-release }
      - run: cargo nextest run --workspace --release

  rust-bench:
    needs: build-rust
    if: <Rust-touching paths only>
    steps: ...

  e2e:
    needs: build-rust
    steps:
      - uses: actions/download-artifact@v4
        with: { name: kars-binaries, path: ./bin }
      # Dockerfile becomes:
      #   FROM gcr.io/distroless/static:nonroot
      #   COPY bin/kars-controller /usr/local/bin/
      # No cargo build inside Docker at all
      - run: docker buildx build -f controller/Dockerfile.distroless .
```

**Result**:
- **1 Rust compile per CI run** (instead of 5)
- **Docker builds: ~30 sec each** (just COPY a binary into distroless)
- **Production images: ~10 MB** (instead of ~50 MB Azure Linux base)
- **Total Rust-touching PR: 8–10 min** (instead of 30 min)

**Concrete prerequisites**:
1. Add `vendored-openssl` feature to crates that pull `openssl-sys` (5 min — Cargo.toml edits)
2. Verify `cargo build --release --target x86_64-unknown-linux-musl --workspace` works locally (15 min)
3. Rewrite each Rust Dockerfile to a 5-line distroless variant (1 hr)
4. Restructure `ci.yml`: `build-rust` first, everything else `needs: build-rust` (2 hr)
5. Delete `image-cache-publish.yml` — the `build-rust` job IS the cache now (10 min)

**Total effort**: ½ day. **Risk**: medium — musl static linking has a few well-known gotchas (`reqwest` with rustls is fine; `tonic`/grpcio sometimes need build tweaks).

#### Option C — `image-cache-publish` becomes the only source of truth (1 day)

Make `image-cache-publish.yml` the only place Rust is compiled. Every other job pulls pre-built images from GHCR.

Pros: zero duplicate work; preserves existing Dockerfiles
Cons: tests run inside Docker (slower iteration); harder to debug cargo errors (buried in Docker layers)

### Comparison table

| Property | Option A | **Option B** ⭐ | Option C |
|---|---|---|---|
| Total Rust compiles per PR | 2 (host + Docker) | **1** | 1 |
| Docker image size | Same (~50 MB) | **~10 MB distroless** | Same |
| Test latency | Same | **Same** (host tests) | Slower (Docker build first) |
| Effort | 4 hr | **½ day** | 1 day |
| Debugging cargo errors | Easy | **Easy** | Hard |
| State of the art? | Good | **Yes — current best practice** | Niche |

### Recommended sequence (Option B)

1. Land Phase 1 quick wins from the main plan above (path-gate bench, delete no-ops, merge Trivy) — 1–2 hr, drops PR time to ~15 min
2. Apply Option B as a separate PR — ½ day, drops PR time to ~8–10 min

The Phase 1 + Option B combination hits the "state of the art" target: doc-only <2 min, CLI/TS <4 min, Rust <10 min.
