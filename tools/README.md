# tools/

Repo-local tooling that is **not** shipped in any deployable artifact.

## [`item-manifest`](./item-manifest/)

A `syn`-based Rust binary that walks `.rs` files and emits a JSON manifest
of every item (fn, struct, const, static, enum, type alias, impl block).
For each function it records a `body_sha256` over the `{…}` block only —
visibility changes (`pub(super)`, `pub(crate)`) do not affect the hash.

Used as a gate for large mechanical refactors: capture a baseline manifest
before the refactor, then compare against a post-refactor manifest with
[`tools/drift/drift.py`](./drift/drift.py). Zero drift (or a documented,
allowlisted drift) is the go/no-go signal.

Build and run:

    cargo build --release -p item-manifest
    tools/item-manifest/target/release/item-manifest routes <path-or-dir>

## [`drift/`](./drift/)

Drift checker + baseline snapshots + allowlists for each refactor that
used the proof framework.

    python3 tools/drift/drift.py <baseline.json> <post.json> [allowlist.txt]

Current baselines:

- `baselines/routes-pre-q1-e4d61c4.json` — `inference-router/src/routes.rs`
  at commit `e4d61c4` (last commit before the q1 split started). Paired
  with `allowlist-q1.txt`.
