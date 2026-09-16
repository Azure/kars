# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import io
import pathlib
import ssl
import tarfile
import unittest
from unittest.mock import patch

from bridge_image_contract import check_config, check_rootfs, smoke_gateway


class ImageContractTests(unittest.TestCase):
    def rootfs(self, files):
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w") as archive:
            for name, data in files.items():
                entry = tarfile.TarInfo(name)
                entry.size = len(data)
                archive.addfile(entry, io.BytesIO(data))
        stream.seek(0)
        with tarfile.open(fileobj=stream, mode="r|") as archive:
            return check_rootfs(archive)

    def base(self):
        certs = ssl.create_default_context().get_ca_certs(binary_form=True)
        self.assertTrue(certs, "Tests need the host's real CA trust store")
        return {
            "usr/lib/os-release": b'ID=azurelinux\nVERSION_ID="3.0"\nNAME="Azure Linux"\n',
            "etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem":
                ssl.DER_cert_to_PEM_cert(certs[0]).encode(),
        }

    def test_requires_actual_azure_linux_and_real_ca(self):
        self.assertEqual(self.rootfs(self.base())["distrolessToolInventory"], "passed")
        files = self.base()
        bundle = "etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem"
        files[bundle] = "# Autorit\u00e9 de certification\n".encode() + files[bundle]
        self.assertTrue(self.rootfs(files)["caBundles"])
        for data in (b"ID=debian\nVERSION_ID=12", b"ID=azurelinux\nVERSION_ID=4.0"):
            files = self.base()
            files["usr/lib/os-release"] = data
            with self.assertRaisesRegex(ValueError, "Azure Linux 3"):
                self.rootfs(files)
        with self.assertRaisesRegex(ValueError, "os-release"):
            self.rootfs({})
        with self.assertRaisesRegex(ValueError, "CA trust"):
            self.rootfs({"etc/os-release": self.base()["usr/lib/os-release"]})

    def test_rejects_shells_managers_and_unused_npm_packages(self):
        for path in ("bin/sh", "usr/bin/bash", "usr/bin/tdnf", "usr/bin/rpm",
                     "usr/local/bin/npm", "usr/local/bin/cargo",
                     "usr/local/lib/node_modules/npm/node_modules/tar/package.json"):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "contains"):
                self.rootfs({**self.base(), path: b"not allowed"})

    def test_keeps_application_dependencies_and_package_inventory(self):
        files = {**self.base(), "var/lib/rpm/rpmdb.sqlite": b"inventory",
                 "app/node_modules/sharp/package.json": b"{}"}
        self.assertEqual(self.rootfs(files)["distrolessToolInventory"], "passed")

    def test_refuses_traversal_and_fake_certificates(self):
        with self.assertRaisesRegex(ValueError, "archive path"):
            self.rootfs({"../etc/os-release": b"invalid"})
        files = self.base()
        files["etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem"] = b"not a certificate"
        with self.assertRaisesRegex(ValueError, "CA certificates"):
            self.rootfs(files)
        files = self.base()
        files["etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem"] += b"-----BEGIN CERTIFICATE-----"
        with self.assertRaisesRegex(ValueError, "CA certificates"):
            self.rootfs(files)

    def test_requires_real_nonroot_direct_application_entrypoint(self):
        config = {"Os": "linux", "Architecture": "amd64", "Id": "sha256:test",
                  "Config": {"User": "10001:10001",
                             "Entrypoint": ["/usr/local/bin/kars-bridge-bff"]}}
        self.assertEqual(check_config(config, "bff")["user"], "10001:10001")
        for user in ("", "root", "0", "10001"):
            with self.subTest(user=user), self.assertRaisesRegex(ValueError, "UID/GID"):
                check_config({**config, "Config": {**config["Config"], "User": user}}, "bff")
        with self.assertRaisesRegex(ValueError, "directly"):
            check_config({**config, "Config": {**config["Config"],
                                               "Entrypoint": ["/bin/sh", "-c"]}}, "bff")

    def test_native_lane_builds_the_shipping_bff_dockerfile(self):
        root = pathlib.Path(__file__).resolve().parents[1]
        workflow = (root / ".github/workflows/bridge-native.yml").read_text()
        self.assertIn("docker build --file bff/Dockerfile", workflow)
        self.assertNotIn("Dockerfile.bff", workflow)
        dockerfile = (root / "bridge/bff/Dockerfile").read_text()
        self.assertIn("FROM ${AZURELINUX_DISTROLESS} AS runtime", dockerfile)
        runtime = dockerfile.split("FROM ${AZURELINUX_DISTROLESS} AS runtime", 1)[1]
        self.assertNotIn("RUN ", runtime)
        self.assertIn("USER 10001:10001", runtime)

    def test_each_shipping_image_has_a_fatal_complete_scan(self):
        root = pathlib.Path(__file__).resolve().parents[1]
        workflow = (root / ".github/workflows/bridge-ci.yml").read_text()
        for image in ("bff", "web", "gateway"):
            marker = f"image-ref: kars-bridge-{image}-qualification:latest"
            self.assertEqual(workflow.count(marker), 1)
            scan = workflow.split(marker, 1)[1].split("      - name:", 1)[0]
            for requirement in ("scanners: vuln", "severity: HIGH,CRITICAL",
                                "ignore-unfixed: false", "exit-code: '1'",
                                "format: json"):
                self.assertIn(requirement, scan)
        self.assertNotIn("continue-on-error:", workflow)

    def test_gateway_requires_sdk_listener_and_health_without_external_network(self):
        def execute(*args):
            if args[0] == "run":
                self.assertIn("--read-only", args)
                self.assertEqual(args[args.index("--network") + 1], "none")
                return "owned-test-container"
            if args[0] == "exec":
                self.assertEqual(args[1:4], ("owned-test-container", "node", "-e"))
                self.assertIn("127.0.0.1:3978/__image_startup_probe__", args[4])
                self.assertIn("assert.equal(app.status, 404)", args[4])
                self.assertIn("assert.equal(health.status, 200)", args[4])
                self.assertIn("teamsAuthenticationQualified: false", args[4])
                return '{"health":"ok","teamsAuthenticationQualified":false}'
            self.assertEqual(args, ("rm", "--force", "owned-test-container"))
            return ""

        with patch("bridge_image_contract.docker", side_effect=execute):
            self.assertFalse(smoke_gateway("test-image")["teamsAuthenticationQualified"])
        with patch("bridge_image_contract.docker",
                   side_effect=["owned-test-container", RuntimeError("startup failed"), ""]) as tool:
            with self.assertRaisesRegex(RuntimeError, "startup failed"):
                smoke_gateway("test-image")
            self.assertEqual(tool.call_args.args, ("rm", "--force", "owned-test-container"))


if __name__ == "__main__":
    unittest.main()
