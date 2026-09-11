# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import os
from pathlib import Path
import subprocess
import unittest

from git_fixture import GitFixture

GATE = Path(__file__).resolve().parents[1] / "no-custom-crypto.sh"


class CryptoGateTests(GitFixture):
    def gate(self):
        return subprocess.run(
            ["bash", str(GATE)], cwd=self.root, text=True, capture_output=True,
            env={**os.environ, "BASE_REF": self.base}, timeout=30,
        )

    def test_exact_standard_digest_adapter_is_the_only_new_allowed_file(self):
        self.write("bridge/bff/src/providers/signing.rs", "use sha2::{Digest, Sha256};\n")
        self.commit()
        result = self.gate()
        self.assertEqual((result.returncode, result.stderr), (0, ""))

    def test_filename_prefixes_cannot_impersonate_allowlisted_adapters(self):
        for name in ("controller/src/providers/signing.rs-extra.rs",
                     "bridge/bff/src/providers/signing.rs-extra.rs",
                     "bridge/bff/src/providers/signing.rs/child.rs"):
            self.write(name, "use sha2::{Digest, Sha256};\n")
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1)
        for name in ("controller/src/providers/signing.rs-extra.rs",
                     "bridge/bff/src/providers/signing.rs-extra.rs",
                     "bridge/bff/src/providers/signing.rs/child.rs"):
            self.assertIn(f"fail: {name} introduces custom crypto", result.stderr)

    def test_application_derivation_and_unreviewed_provider_files_remain_blocked(self):
        for name in ("bridge/bff/src/routes/credential_review.rs",
                     "bridge/bff/src/providers/another.rs",
                     "bridge/bff/src/routes/receipts/verification.rs"):
            self.write(name, "use sha2::{Digest, Sha256};\n")
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr.count("introduces custom crypto"), 3)

    def test_existing_explicit_directory_and_file_contracts_still_work(self):
        self.write("controller/src/providers/signing.rs", "use sha2::{Digest, Sha256};\n")
        self.write("controller/src/mesh_peer/nested/wrapper.rs", "use ed25519_dalek::Signer;\n")
        self.commit()
        result = self.gate()
        self.assertEqual((result.returncode, result.stderr), (0, ""))


if __name__ == "__main__":
    unittest.main()
