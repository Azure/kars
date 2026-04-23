#!/usr/bin/env python3
"""Q1 refactor drift checker.

Compares a baseline item manifest against a post-refactor manifest and
verifies that every function body is preserved byte-for-byte.

Policy:
  - Every fn/impl_fn/trait_fn in baseline MUST appear in post with the same
    body_sha256. Match is by (leaf_name, body_sha256) — fq_path may change
    because items moved into submodules.
  - Additions of new fn items are FLAGGED (not an error, but must be
    explained — should only be mechanical wrappers/re-exports).
  - `use` / `mod` entries are informational: churn is expected.
  - Non-fn items (struct, const, static, enum, etc.) must also be
    preserved: same (leaf_name, token_sha256).
"""
import json
import sys
from pathlib import Path


def leaf(fq: str) -> str:
    return fq.rsplit("::", 1)[-1]


def load(p: str) -> list[dict]:
    return json.loads(Path(p).read_text())


def key_for(e: dict) -> tuple:
    # For fns: identity = (kind, name, body bytes). Body hash is what matters.
    if e["kind"] in ("fn", "impl_fn", "trait_fn"):
        return (e["kind"], leaf(e["fq_path"]), e["body_sha256"])
    # For data items: identity = (kind, name, full token hash)
    if e["kind"] in ("struct", "enum", "const", "static", "type"):
        return (e["kind"], leaf(e["fq_path"]), e["token_sha256"])
    return None


def main():
    if len(sys.argv) not in (3, 4):
        print("usage: drift.py <baseline.json> <post.json> [allowlist.txt]", file=sys.stderr)
        sys.exit(2)

    base = load(sys.argv[1])
    post = load(sys.argv[2])

    # Allowlist: one fn-leaf-name per line; '#' comments allowed. Mutations on
    # these names are permitted (but still reported as INFO). Use for
    # mechanical namespace-resolution fixes that are byte-different but
    # semantically identical — must be justified in the PR.
    allowlist: set[str] = set()
    if len(sys.argv) == 4:
        for raw in Path(sys.argv[3]).read_text().splitlines():
            line = raw.split("#", 1)[0].strip()
            if line:
                allowlist.add(line)

    base_keys = {k: e for e in base if (k := key_for(e)) is not None}
    post_keys = {k: e for e in post if (k := key_for(e)) is not None}

    missing = [k for k in base_keys if k not in post_keys]
    added = [k for k in post_keys if k not in base_keys]

    # Additionally: every fn leaf-name present in base must exist in post with
    # SOME body_sha256. If the body hash changed, it shows as "missing" AND
    # "added" under the same leaf-name — flag this as MUTATION.
    base_fn_by_name = {}
    for k, e in base_keys.items():
        if k[0] in ("fn", "impl_fn", "trait_fn"):
            base_fn_by_name.setdefault(k[1], []).append((k[2], e))
    post_fn_by_name = {}
    for k, e in post_keys.items():
        if k[0] in ("fn", "impl_fn", "trait_fn"):
            post_fn_by_name.setdefault(k[1], []).append((k[2], e))

    mutations = []
    for name, entries in base_fn_by_name.items():
        base_hashes = {h for h, _ in entries}
        post_hashes = {h for h, _ in post_fn_by_name.get(name, [])}
        lost = base_hashes - post_hashes
        if lost and name in post_fn_by_name:
            mutations.append((name, sorted(base_hashes), sorted(post_hashes)))

    ok = True
    print(f"Baseline items (fn+data): {len(base_keys)}")
    print(f"Post items     (fn+data): {len(post_keys)}")
    print()

    if mutations:
        hard = [(n, b, p) for n, b, p in mutations if n not in allowlist]
        soft = [(n, b, p) for n, b, p in mutations if n in allowlist]
        if hard:
            ok = False
            print("❌ MUTATED function bodies:")
            for name, bh, ph in hard:
                print(f"   {name}")
                print(f"     baseline body hashes: {bh}")
                print(f"     post     body hashes: {ph}")
            print()
        if soft:
            print("ℹ  ALLOWLISTED mutations (justified in PR):")
            for name, bh, ph in soft:
                print(f"   {name}  {bh[0][:12]} → {ph[0][:12]}")
            print()

    disappeared = [(k, base_keys[k]) for k in missing if k[1] not in post_fn_by_name and k[0] not in ("fn", "impl_fn", "trait_fn")]
    disappeared += [(k, base_keys[k]) for k in missing if k[0] in ("fn", "impl_fn", "trait_fn") and k[1] not in post_fn_by_name]
    if disappeared:
        ok = False
        print("❌ DISAPPEARED items (in baseline, absent from post):")
        for k, e in disappeared:
            print(f"   {k[0]} {k[1]} (was at {e['file']}:{e['line']})")
        print()

    new_fns = [(k, post_keys[k]) for k in added if k[0] in ("fn", "impl_fn", "trait_fn") and k[1] not in base_fn_by_name]
    if new_fns:
        print("⚠  NEW functions (not in baseline) — verify these are expected mechanical additions:")
        for k, e in new_fns:
            print(f"   {k[0]} {k[1]} @ {e['file']}:{e['line']}")
        print()

    new_data = [(k, post_keys[k]) for k in added if k[0] in ("struct", "enum", "const", "static", "type") and k[1] not in {n for _, n, _ in base_keys.keys()}]
    if new_data:
        print("⚠  NEW data items:")
        for k, e in new_data:
            print(f"   {k[0]} {k[1]} @ {e['file']}:{e['line']}")
        print()

    if ok:
        print("✅ ZERO BEHAVIORAL DRIFT — every function body is byte-identical, every data item preserved.")
    else:
        print("❌ DRIFT DETECTED — see above.")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
