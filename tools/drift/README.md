# drift — behavioral-equivalence proof for mechanical refactors

`drift.py` compares two item manifests (produced by
[`../item-manifest`](../item-manifest/)) and reports whether every function
body is byte-identical.

## Usage

    python3 tools/drift/drift.py <baseline.json> <post.json> [allowlist.txt]

Exit code:

- `0` — zero drift, or every mutation is on the allowlist.
- `1` — unallowed drift detected (diff printed on stderr).

## How to use on a new refactor

1. **Capture baseline** — before the first change:

       tools/item-manifest/target/release/item-manifest routes \
           inference-router/src/routes/ > baselines/routes-<label>.json

2. **Make the refactor**, commit-by-commit.

3. **Verify at each wave**:

       tools/item-manifest/target/release/item-manifest routes \
           inference-router/src/routes/ > /tmp/post.json
       python3 tools/drift/drift.py \
           tools/drift/baselines/routes-<label>.json /tmp/post.json

4. **Document every allowlisted mutation.** If you must change a function
   body (e.g. namespace-resolution), add it to
   `tools/drift/allowlist-<refactor>.txt` with the justification as a
   comment, and the drift checker will pass with an "ℹ ALLOWLISTED" line.

## Current baselines / allowlists

| Refactor | Baseline | Allowlist |
|----------|----------|-----------|
| q1 routes split | `baselines/routes-pre-q1-e4d61c4.json` | `allowlist-q1.txt` |
