# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from contracts import module_inventory

ROOT = Path(__file__).resolve().parents[1]
CRYPTO = "golang.org/x/crypto"
FIXED = "v0.58.1-0.20261009181718-2f1f834330b2"
FALLBACK = "v0.58.0"


class NestedResolverContracts(unittest.TestCase):
    def run_nested_resolution(self, request, *, remove_repin=False, fail_repin=False):
        script = (ROOT / "scripts/generate-locks.sh").read_text()
        nested = script[script.index("crypto_request="):script.index("# Keep the released root")]
        if remove_repin:
            self.assertEqual(nested.count('    go get "$crypto_request"\n'), 1)
            nested = nested.replace('    go get "$crypto_request"\n', "")
        with tempfile.TemporaryDirectory(prefix=".resolver-contract-", dir=ROOT) as directory:
            work = Path(directory)
            for name in ("source/api/v2", "packaging/locks", "out", "bin"):
                (work / name).mkdir(parents=True)
            (work / "packaging/locks/requests.txt").write_text(f"{CRYPTO}@{request}\n")
            nested = nested.replace("/packaging", str(work / "packaging")).replace("/out", str(work / "out"))
            (work / "nested.sh").write_text('set -eu\nGO_VERSION=go1.27.3\n' + nested)
            shim = work / "bin/go"
            # Model the observed run 41997973603 behavior: tidy removes the unused
            # crypto requirement, exposing x/net's v0.58.0 graph selection. This
            # checks actual shell orchestration, not Go resolution or qualification.
            shim.write_text('''#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$GO_CALLS"
case "$*" in
  'mod edit '*|'mod download'|'mod verify') ;;
  'mod tidy') printf '%s\n' 'v0.58.0' > "$GO_SELECTED" ;;
  'get '*)
    if test "$#" = 2 && test "${FAIL_REPIN:-0}" = 1; then
      echo 'simulated resolver failure retaining crypto pin' >&2
      exit 19
    fi
    for request in "$@"; do
      case "$request" in
        golang.org/x/crypto@*) printf '%s\n' "${request#*@}" > "$GO_SELECTED" ;;
      esac
    done ;;
  'list -m -json all')
    printf '{"Path":"golang.org/x/crypto","Version":"%s"}\n' "$(cat "$GO_SELECTED")" ;;
  *) echo "unexpected test Go command: $*" >&2; exit 20 ;;
esac
''')
            shim.chmod(0o755)
            env = {**os.environ, "PATH": str(work / "bin") + os.pathsep + os.environ["PATH"],
                   "GO_CALLS": str(work / "calls"), "GO_SELECTED": str(work / "selected"),
                   "FAIL_REPIN": "1" if fail_repin else "0"}
            result = subprocess.run(["sh", str(work / "nested.sh")], cwd=work / "source",
                                    env=env, capture_output=True, text=True)
            inventory = work / "out/api-modules.json"
            return result, (work / "calls").read_text().splitlines(), (
                module_inventory(inventory.read_text()) if inventory.exists() else None)

    def test_real_hosted_tidy_fallback_is_reproduced_and_pin_is_restored(self):
        failed, _, old = self.run_nested_resolution(FIXED, remove_repin=True)
        self.assertEqual(failed.returncode, 0, failed.stderr)
        self.assertEqual(old[CRYPTO]["Version"], FALLBACK)
        self.assertNotEqual(old[CRYPTO]["Version"], FIXED)
        for requested in (FIXED, FALLBACK):
            with self.subTest(requested=requested):
                result, calls, resolved = self.run_nested_resolution(requested)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(resolved[CRYPTO]["Version"], requested)
                self.assertEqual(calls, [
                    "mod edit -go=1.26.0 -toolchain=go1.27.3",
                    f"get google.golang.org/grpc@v1.84.0 {CRYPTO}@{requested} golang.org/x/net@v0.62.0",
                    "mod tidy", f"get {CRYPTO}@{requested}", "mod download", "mod verify",
                    "list -m -json all",
                ])

    def test_failed_policy_pin_stops_before_verification_or_inventory(self):
        result, calls, inventory = self.run_nested_resolution(FIXED, fail_repin=True)
        self.assertEqual(result.returncode, 19, result.stderr)
        self.assertNotIn("mod download", calls)
        self.assertNotIn("mod verify", calls)
        self.assertIsNone(inventory)


if __name__ == "__main__":
    unittest.main()
