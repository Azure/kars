# item-manifest

`syn`-based extractor for behavioral-equivalence proofs on large mechanical
refactors. See [`../README.md`](../README.md).

## Usage

    cargo build --release -p item-manifest
    ./target/release/item-manifest routes <file-or-directory> > manifest.json

Then compare with [`../drift/drift.py`](../drift/drift.py).

## What the manifest contains

Each entry has:

- `kind` — one of `fn`, `impl_fn`, `trait_fn`, `struct`, `enum`, `const`,
  `static`, `type`, `use`, `mod`, `impl`, `trait`.
- `fq_path` — fully-qualified path (`mod::submod::Item` or `impl Ty::method`).
- `file` + `line` — source location.
- `token_sha256` — hash of the entire item's token stream (visibility,
  attributes, signature, body).
- `body_sha256` — (functions only) hash of the `{…}` block only. Stable
  across `pub` → `pub(super)` changes, function moves between files, etc.

## Why body-hash, not textual diff?

- Trailing-comma / whitespace churn does not affect the hash.
- Moving a function into a submodule does not affect the hash.
- Changing visibility or attributes does not affect the hash.
- Changing any statement inside the body **does** affect the hash.

This makes the drift check exactly right for mechanical refactors:
it flags real behavior changes and ignores cosmetic noise.

## Not a workspace member

`tools/item-manifest` is declared under `workspace.exclude` in the root
`Cargo.toml` so it is not built by `cargo build --workspace` and does not
influence the main lockfile. Build it explicitly when needed.
