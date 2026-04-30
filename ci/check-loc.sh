#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# ci/check-loc.sh — enforces internal Phase 1 plan §4.2 LOC budget.
#
# Fails if:
#   - A budgeted file exceeds its active-phase cap.
#   - A budgeted file touched by this PR *grew* vs. BASE_REF.
#   - Any non-budgeted file newly added by this PR exceeds the per-language new-file cap.
#
# Overrides: a single line comment `// ci:loc-ok` on the file's first 20 lines
# permits the file to exceed its new-file cap. Budgeted-file caps cannot be
# overridden inline (requires a ci/loc-budget.yaml update, review-gated).
#
# Env:
#   BASE_REF  — git ref to diff against (default: origin/main).
#   VERBOSE=1 — print per-file budget status.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
BUDGET="$REPO_ROOT/ci/loc-budget.yaml"
BASE_REF="${BASE_REF:-origin/main}"

if ! command -v python3 >/dev/null 2>&1; then
  echo "check-loc.sh: python3 required" >&2
  exit 2
fi

python3 - "$BUDGET" "$BASE_REF" "${VERBOSE:-0}" <<'PY'
import os, re, subprocess, sys, pathlib

budget_path, base_ref, verbose = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
repo_root = pathlib.Path(
    subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip()
)

# -- minimal YAML parse (no PyYAML dep). Handles the shape we control. --
def parse_budget(path):
    text = pathlib.Path(path).read_text()
    global_cap = {}
    files = []
    active_phase = "phase0"
    current_file = None
    in_global = False
    in_files = False
    for raw in text.splitlines():
        line = raw.rstrip()
        if not line or line.lstrip().startswith("#"):
            continue
        if line.startswith("global:"):
            in_global, in_files = True, False
            continue
        if line.startswith("files:"):
            in_global, in_files = False, True
            continue
        if line.startswith("active_phase:"):
            active_phase = line.split(":", 1)[1].strip()
            continue
        if in_global and line.startswith("  ") and ":" in line:
            k, v = line.strip().split(":", 1)
            v = v.strip().strip('"')
            if v.isdigit():
                global_cap[k] = int(v)
            elif v in ("true", "false"):
                global_cap[k] = (v == "true")
            else:
                global_cap[k] = v
        if in_files:
            if line.startswith("  - path:"):
                if current_file:
                    files.append(current_file)
                current_file = {"path": line.split(":", 1)[1].strip()}
            elif current_file is not None and line.startswith("    ") and ":" in line:
                k, v = line.strip().split(":", 1)
                v = v.strip().strip('"')
                if v.isdigit():
                    current_file[k] = int(v)
                else:
                    current_file[k] = v
    if current_file:
        files.append(current_file)
    return global_cap, files, active_phase

global_cap, files, active_phase = parse_budget(budget_path)
budgeted = {f["path"]: f for f in files}

def run(cmd):
    return subprocess.check_output(cmd, text=True, cwd=repo_root).splitlines()

try:
    changed = run(["git", "diff", "--name-only", f"{base_ref}...HEAD"])
except subprocess.CalledProcessError:
    # new branch without base_ref — fall back to staged + working-tree diff
    changed = run(["git", "diff", "--name-only", "HEAD"])
    changed += run(["git", "diff", "--name-only", "--cached"])

added = []
try:
    added = run(["git", "diff", "--name-only", "--diff-filter=A", f"{base_ref}...HEAD"])
except subprocess.CalledProcessError:
    pass

def _strip_copyright_header(text_bytes):
    # Skip an optional shebang, then up to one blank line, then the prescribed
    # 2-line "Copyright (c) Microsoft Corporation. / Licensed under the MIT License."
    # header (with an optional trailing blank line). The header is OSS boilerplate
    # mandated by Microsoft OSPO (see ci/check-copyright-headers.sh) and must not
    # count against the §4.2 LOC budget for real maintainable code.
    try:
        text = text_bytes.decode("utf-8", errors="ignore")
    except Exception:
        return text_bytes.count(b"\n") + (0 if text_bytes.endswith(b"\n") or not text_bytes else 1)
    lines = text.splitlines()
    i = 0
    if i < len(lines) and lines[i].startswith("#!"):
        i += 1
    # Allow an optional blank line after the shebang
    if i < len(lines) and lines[i].strip() == "" and i + 1 < len(lines) and "Copyright (c) Microsoft Corporation" in lines[i + 1]:
        i += 1
    if (
        i + 1 < len(lines)
        and "Copyright (c) Microsoft Corporation" in lines[i]
        and "Licensed under the MIT License" in lines[i + 1]
    ):
        i += 2
        # Strip the customary blank line after the header
        if i < len(lines) and lines[i].strip() == "":
            i += 1
    return max(0, len(lines) - i)

def line_count(path):
    p = repo_root / path
    if not p.is_file():
        return 0
    with p.open("rb") as fh:
        return _strip_copyright_header(fh.read())

def line_count_at_ref(path, ref):
    try:
        blob = subprocess.check_output(["git", "show", f"{ref}:{path}"], cwd=repo_root)
        return _strip_copyright_header(blob)
    except subprocess.CalledProcessError:
        return None  # file didn't exist at ref

def has_override(path):
    p = repo_root / path
    if not p.is_file():
        return False
    try:
        with p.open("r", encoding="utf-8", errors="ignore") as fh:
            head = [next(fh, "") for _ in range(20)]
    except Exception:
        return False
    return any(global_cap.get("override_comment", "// ci:loc-ok") in l for l in head)

def phase_cap(entry):
    # Walk active_phase down to phase0; return first cap found.
    order = ["phase4", "phase3", "phase2", "phase1", "phase0"]
    idx = order.index(active_phase) if active_phase in order else len(order) - 1
    for p in order[idx:]:
        k = f"{p}_cap"
        if k in entry:
            return entry[k], p
    return None, None

def new_file_cap_for(path):
    if path.endswith(".rs"):
        return global_cap.get("new_file_cap_rust", 800), "rust"
    if path.endswith(".ts") or path.endswith(".tsx"):
        return global_cap.get("new_file_cap_ts", 800), "ts"
    return None, None

failures = []
warnings = []

for path in sorted(set(changed)):
    if not path:
        continue
    if path in budgeted:
        entry = budgeted[path]
        cap, cap_phase = phase_cap(entry)
        current = line_count(path)
        if cap is not None and current > cap:
            failures.append(
                f"{path}: {current} LOC exceeds {cap_phase} cap of {cap} "
                f"(baseline {entry.get('baseline_2026_04_24', '?')}). "
                f"Decompose before growing."
            )
        if global_cap.get("touched_must_shrink", False) and not entry.get("allow_grow", False):
            base = line_count_at_ref(path, base_ref)
            if base is not None and current > base:
                failures.append(
                    f"{path}: grew from {base} -> {current} LOC. "
                    f"Per §4.3 'touched code pays its decomposition debt' — "
                    f"touched budgeted files must not grow."
                )
        if verbose:
            print(f"[budget] {path}: {current} / cap {cap} ({cap_phase})")
    elif path in added:
        cap, lang = new_file_cap_for(path)
        if cap is None:
            continue
        current = line_count(path)
        if current > cap and not has_override(path):
            failures.append(
                f"{path}: new file is {current} LOC, exceeds {lang} cap of {cap}. "
                f"Split or add '{global_cap.get('override_comment')}' with reviewer sign-off."
            )
        elif lang == "ts" and current > global_cap.get("warn_file_cap_ts", 600):
            warnings.append(f"{path}: new TS file is {current} LOC (warn threshold {global_cap.get('warn_file_cap_ts')}).")

for w in warnings:
    print(f"warn: {w}")
for f in failures:
    print(f"fail: {f}", file=sys.stderr)

sys.exit(1 if failures else 0)
PY
