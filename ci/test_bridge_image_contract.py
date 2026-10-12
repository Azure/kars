# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import io
import json
import pathlib
import shlex
import ssl
import subprocess
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
        self.assertIn("docker build --file bff/Dockerfile --tag kars-native-bff:latest ..", workflow)
        self.assertNotIn("Dockerfile.bff", workflow)
        dockerfile = (root / "bridge/bff/Dockerfile").read_text()
        self.assertIn("FROM ${AZURELINUX_DISTROLESS} AS runtime", dockerfile)
        runtime = dockerfile.split("FROM ${AZURELINUX_DISTROLESS} AS runtime", 1)[1]
        self.assertNotIn("RUN ", runtime)
        self.assertIn("USER 10001:10001", runtime)

    def test_bff_build_preserves_shared_source_layout(self):
        root = pathlib.Path(__file__).resolve().parents[1]
        dockerfile = (root / "bridge/bff/Dockerfile").read_text()
        builder = dockerfile.split("FROM ${AZURELINUX_DISTROLESS} AS runtime", 1)[0]
        for instruction in ("WORKDIR /src", "COPY bridge/bff/ bridge/bff/",
                            "COPY shared/ shared/", "WORKDIR /src/bridge/bff"):
            self.assertIn(instruction, builder)
            self.assertLess(builder.index(instruction), builder.index("RUN cargo build"))
        module = root / "bridge/bff/src/kars/../../../../shared/execution_plan.rs"
        self.assertTrue(module.is_file())
        workflow = (root / ".github/workflows/bridge-ci.yml").read_text()
        self.assertIn("docker build --file Dockerfile --tag kars-bridge-bff-qualification:latest ../..", workflow)
        makefile = (root / "bridge/Makefile").read_text()
        bff_builds = [line for line in makefile.splitlines() if "docker build -f bff/Dockerfile" in line]
        self.assertEqual(len(bff_builds), 2)
        self.assertTrue(all(line.endswith(" ..") for line in bff_builds))

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


class DispatcherPackagingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        root = pathlib.Path(__file__).resolve().parents[1]
        cls.recipe = (root / "mesh-plugin/Dockerfile.dispatcher").read_text()
        cls.build, _, remaining = cls.recipe.partition(
            "FROM mcr.microsoft.com/azurelinux/distroless/base:3.0 AS runtime-base")
        cls.libraries, _, runtime = remaining.partition("FROM runtime-base AS runtime")
        cls.runtime = [line.strip() for line in runtime.replace("\\\n", "").splitlines()
                       if line.strip() and not line.lstrip().startswith("#")]

    def test_keeps_locked_vendor_build_and_existing_secret_cache(self):
        for instruction in (
            "FROM node:22-bookworm-slim AS build",
            "COPY vendor/agt/*.tgz vendor/agt/SHA256SUMS ./vendor/agt/",
            "COPY mesh-plugin/package.json mesh-plugin/package-lock.json mesh-plugin/tsconfig.json mesh-plugin/openclaw.plugin.json ./mesh-plugin/",
            "COPY mesh-plugin/src ./mesh-plugin/src",
            "COPY mesh-plugin/skills ./mesh-plugin/skills",
            "RUN cd vendor/agt && sha256sum -c SHA256SUMS",
            "--mount=type=cache,id=kars-mission-dispatcher-npm,target=/root/.npm",
            "--mount=type=secret,id=npmrc,target=/root/.npmrc",
            "npm ci --ignore-scripts --prefer-offline --no-audit --no-fund",
            "npm run build && npm prune --omit=dev --ignore-scripts --no-audit --no-fund",
        ):
            with self.subTest(instruction=instruction):
                self.assertIn(instruction, self.build)

    def test_runtime_copies_only_microsoft_libraries_node_and_application(self):
        self.assertEqual([line for line in self.recipe.splitlines() if line.startswith("FROM ")], [
            "FROM node:22-bookworm-slim AS build",
            "FROM mcr.microsoft.com/azurelinux/distroless/base:3.0 AS runtime-base",
            "FROM mcr.microsoft.com/azurelinux/base/core:3.0 AS runtime-libs",
            "FROM runtime-base AS runtime",
        ])
        self.assertEqual([line for line in self.runtime if line.startswith("COPY ")], [
            "COPY --from=runtime-libs /runtime-libs/ /",
            "COPY --from=build /usr/local/bin/node /usr/local/bin/node",
            "COPY --from=build /usr/local/LICENSE /usr/local/share/licenses/node/LICENSE",
            "COPY --from=build /build/mesh-plugin/package.json ./package.json",
            "COPY --from=build /build/mesh-plugin/dist ./dist",
            "COPY --from=build /build/mesh-plugin/node_modules ./node_modules",
        ])
        allowed = {"ENV", "WORKDIR", "COPY", "RUN", "USER", "EXPOSE", "ENTRYPOINT", "CMD"}
        self.assertTrue(all(line.split()[0] in allowed for line in self.runtime))
        runs = [json.loads(line[4:]) for line in self.runtime if line.startswith("RUN ")]
        self.assertEqual(len(runs), 2)
        self.assertTrue(all(command[0] == "node" for command in runs))

    def test_preserves_microsoft_payload_inventory_and_abi_checks(self):
        for instruction in (
            "COPY --from=runtime-base /var/lib/rpmmanifest/ /runtime-libs/var/lib/rpmmanifest/",
            "rpm -V libstdc++",
            'grep -Fx "$(rpm -q libgcc)" /runtime-libs/var/lib/rpmmanifest/container-manifest-1',
            "rpm -q --qf '%{NAME}-%{VERSION}-' glibc",
            "rpm -q --qf '%{ARCH}' glibc",
            'case "$package" in "$glibc_prefix"?*".$glibc_arch")',
            '[ "$matches" -eq 1 ]',
            "rpm -ql libstdc++ > /package-files",
            'cp -a "$path" "/runtime-libs$directory/"',
            "rpm -q libstdc++ >> /runtime-libs/var/lib/rpmmanifest/container-manifest-1",
            "%{SOURCERPM}",
            ">> /runtime-libs/var/lib/rpmmanifest/container-manifest-2",
        ):
            with self.subTest(instruction=instruction):
                self.assertIn(instruction, self.libraries)
        self.assertNotIn("COPY --from=build", self.libraries)
        self.assertNotIn("rm ", self.libraries)

    def test_preserves_nonroot_command_trust_and_auth_refusal(self):
        env = dict(value.split("=", 1) for line in self.runtime if line.startswith("ENV ")
                   for value in shlex.split(line[4:]))
        ca = "/etc/pki/tls/certs/ca-bundle.crt"
        self.assertEqual(env, {"NODE_ENV": "production", "PATH": "/usr/local/bin",
                               "HOME": "/home/node", "LD_BIND_NOW": "1",
                               "NODE_EXTRA_CA_CERTS": ca, "SSL_CERT_FILE": ca})
        for instruction in ('WORKDIR /opt/kars-mission-dispatcher', 'USER 1000:1000',
                            'EXPOSE 8080', 'ENTRYPOINT []',
                            'CMD ["node", "dist/mission-main.js"]'):
            self.assertIn(instruction, self.runtime)
        user = self.runtime.index("USER 1000:1000")
        setup = [json.loads(line[4:])[-1] for line in self.runtime[:user]
                 if line.startswith("RUN ")]
        self.assertEqual(len(setup), 1)
        for check in ("process.versions.node.split('.')[0] !== '22'", "createSecureContext",
                      "fs.readFileSync(process.env.NODE_EXTRA_CA_CERTS)",
                      "node:x:1000:1000::/home/node:/sbin/nologin",
                      "fs.chownSync('/home/node', 1000, 1000)"):
            self.assertIn(check, setup[0])
        smoke = [json.loads(line[4:])[-1] for line in self.runtime[user + 1:]
                 if line.startswith("RUN ")]
        self.assertEqual(len(smoke), 1)
        for check in ("process.getuid(), 1000", "process.getgid(), 1000",
                      "@microsoft/agent-governance-sdk", "encryption.MeshClient",
                      "encryption.X3DHKeyManager", "./dist/mission-config.js",
                      "assert.throws(() => missionProcessConfig({})",
                      "Mission dispatcher requires explicit development or entra authentication"):
            self.assertIn(check, smoke[0])

    def test_actual_final_runtime_checks_parse_as_node_programs(self):
        for line in self.runtime:
            if line.startswith("RUN "):
                command = json.loads(line[4:])
                mode = "module" if "--input-type=module" in command else "commonjs"
                subprocess.run(["node", "--check", "--input-type=" + mode],
                               input=command[-1], text=True, capture_output=True,
                               check=True, timeout=10)


if __name__ == "__main__":
    unittest.main()
