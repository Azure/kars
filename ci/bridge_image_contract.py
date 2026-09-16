# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Inspect the shipping image, not a test-only Dockerfile or its base label."""

import argparse
import json
import pathlib
import re
import shlex
import ssl
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.request


TOOLS = frozenset({
    "sh", "bash", "ash", "dash", "zsh", "ksh", "busybox",
    "apt", "apt-get", "apk", "tdnf", "dnf", "yum", "rpm",
    "npm", "npx", "yarn", "yarnpkg", "pnpm", "corepack",
    "cargo", "rustc", "gcc", "cc", "make",
})
BIN_DIRS = frozenset({"bin", "sbin", "usr/bin", "usr/sbin",
                      "usr/local/bin", "usr/local/sbin", "busybox"})


def require(condition, message):
    if not condition:
        raise ValueError(message)


def check_config(config, kind):
    require(config["Os"] == "linux", "Shipping image must be Linux")
    require(config["Architecture"] in {"amd64", "arm64"}, "Unsupported image platform")
    runtime = config["Config"]
    require(runtime["User"] == "10001:10001", "Bridge runtime must use UID/GID 10001")
    entry = runtime.get("Entrypoint") or []
    binary = "kars-bridge-bff" if kind == "bff" else "node"
    require(entry and pathlib.PurePosixPath(entry[0]).name == binary,
            "Runtime entrypoint must execute the application directly")
    return {"imageId": config["Id"], "architecture": config["Architecture"],
            "user": runtime["User"], "entrypoint": entry}


def check_rootfs(archive):
    releases = []
    bundles = []
    for member in archive:
        path = pathlib.PurePosixPath(member.name)
        require(not path.is_absolute() and ".." not in path.parts, "Invalid image archive path")
        name = str(path)
        require(not (str(path.parent) in BIN_DIRS and path.name in TOOLS),
                f"Shipping image contains a shell, build tool or package manager: {name}")
        require(not name.startswith(("usr/local/lib/node_modules/npm/",
                                     "usr/lib/node_modules/npm/",
                                     "opt/yarn-", "root/.cargo/")),
                f"Shipping image contains build-tool dependencies: {name}")
        if not member.isfile():
            continue
        is_release = name in {"etc/os-release", "usr/lib/os-release"}
        is_bundle = (name.startswith(("etc/", "usr/share/")) and
                     path.name in {"ca-bundle.crt", "ca-certificates.crt",
                                   "tls-ca-bundle.pem"})
        if not (is_release or is_bundle):
            continue
        require(member.size <= 4 * 1024 * 1024, "Unexpected metadata or CA bundle size")
        data = archive.extractfile(member).read().decode("utf-8")
        if is_release:
            fields = dict(line.split("=", 1) for line in shlex.split(data, comments=True))
            require(fields.get("ID") == "azurelinux" and fields.get("VERSION_ID") == "3.0",
                    "Shipping runtime must be Azure Linux 3")
            releases.append(name)
        else:
            certificates = re.findall(
                r"-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----",
                data, re.DOTALL)
            require(certificates and
                    len(certificates) == data.count("-----BEGIN CERTIFICATE-----") ==
                    data.count("-----END CERTIFICATE-----"), "Malformed runtime CA certificates")
            trust = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            trust.load_verify_locations(cadata="\n".join(certificates))
            require(trust.cert_store_stats()["x509_ca"] > 0, "Empty runtime CA trust store")
            bundles.append(name)
    require(releases, "Actual Azure Linux os-release is required")
    require(bundles, "A readable populated runtime CA trust bundle is required")
    return {"osRelease": releases, "caBundles": bundles, "distrolessToolInventory": "passed"}


def docker(*args):
    return subprocess.run(["docker", *args], check=True, capture_output=True,
                          text=True, timeout=180).stdout.strip()


def inspect_image(image, kind):
    config = json.loads(docker("image", "inspect", image))[0]
    proof = check_config(config, kind)
    container = docker("create", image)
    try:
        with tempfile.TemporaryFile() as output:
            subprocess.run(["docker", "export", container], stdout=output,
                           check=True, timeout=180)
            output.seek(0)
            with tarfile.open(fileobj=output, mode="r|") as archive:
                proof.update(check_rootfs(archive))
    finally:
        docker("rm", container)
    return proof


def smoke_bff(image):
    container = docker("run", "--detach", "--read-only", "--cap-drop", "ALL",
                       "--security-opt", "no-new-privileges", "--pids-limit", "256",
                       "--memory", "1g", "--publish", "127.0.0.1::8081",
                       "--env", "KUBECONFIG=/nonexistent", image)
    try:
        port = docker("port", container, "8081/tcp")
        require(port.startswith("127.0.0.1:") and port.count(":") == 1,
                "Health probe must remain loopback-only")
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        origin = f"http://{port}"
        deadline = time.monotonic() + 60
        while True:
            try:
                with opener.open(origin + "/healthz", timeout=2) as response:
                    health = json.load(response)
                require(health.get("status") == "ok" and
                        health.get("service") == "kars-bridge-bff",
                        "Actual BFF health contract failed")
                break
            except urllib.error.URLError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(1)
        try:
            opener.open(origin + "/readyz", timeout=2)
        except urllib.error.HTTPError as error:
            require(error.code == 503, "Unconfigured BFF must not claim readiness")
            require(json.load(error).get("cluster_configured") is False,
                    "Readiness must disclose missing cluster configuration")
        else:
            raise ValueError("Unconfigured BFF incorrectly reported readiness")
        return {"readonlyHealth": "passed", "unconfiguredReadiness": 503}
    finally:
        docker("rm", "--force", container)


def smoke_gateway(image):
    roles = json.dumps([{"entra_subject": "00000000-0000-4000-8000-000000000003",
                         "bridge_subject": "offline-image-proof",
                         "roles": ["operator", "user"], "name": "Offline image proof"}])
    container = docker(
        "run", "--detach", "--network", "none", "--read-only", "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges", "--pids-limit", "256", "--memory", "1g",
        "--tmpfs", "/tmp:rw,nosuid,noexec,size=16m", "--env", "KUBERNETES_SERVICE_HOST=",
        "--env", "TEAMS_CLIENT_ID=00000000-0000-4000-8000-000000000001",
        "--env", "TEAMS_TENANT_ID=00000000-0000-4000-8000-000000000002",
        "--env", "TEAMS_CLIENT_SECRET=offline-proof-not-a-real-client-secret",
        "--env", "TEAMS_BFF_BASE_URL=http://127.0.0.1:8081",
        "--env", "TEAMS_BFF_INTERNAL_SECRET=offline-proof-not-a-real-hmac-secret",
        "--env", f"TEAMS_ENTRA_ROLE_MAP={roles}", image)
    script = r"""
const assert = require("node:assert/strict");
const fs = require("node:fs");
const tls = require("node:tls");
(async () => {
  assert.equal(process.versions.node.split(".")[0], "22");
  assert.equal(process.getuid(), 10001);
  assert.equal(process.getgid(), 10001);
  const ca = fs.readFileSync(process.env.NODE_EXTRA_CA_CERTS, "utf8");
  assert.match(ca, /BEGIN CERTIFICATE/);
  tls.createSecureContext({ ca });
  let failure;
  for (let attempt = 0; attempt < 30; attempt++) {
    try {
      const app = await fetch("http://127.0.0.1:3978/__image_startup_probe__", {
        signal: AbortSignal.timeout(1000)
      });
      assert.equal(app.status, 404);
      const health = await fetch("http://127.0.0.1:3979/healthz", {
        signal: AbortSignal.timeout(1000)
      });
      assert.equal(health.status, 200);
      assert.equal((await health.json()).status, "ok");
      console.log(JSON.stringify({nodeMajor: 22, readonly: true, network: "none",
        sdkListener: 404, health: "ok", caTlsContext: "passed",
        teamsAuthenticationQualified: false}));
      return;
    } catch (error) {
      failure = error;
      await new Promise(resolve => setTimeout(resolve, 1000));
    }
  }
  throw failure;
})().catch(error => { console.error(error); process.exit(1); });
"""
    try:
        return json.loads(docker("exec", container, "node", "-e", script))
    finally:
        docker("rm", "--force", container)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image")
    parser.add_argument("--kind", choices=("bff", "web", "gateway"), required=True)
    args = parser.parse_args()
    result = inspect_image(args.image, args.kind)
    if args.kind == "bff":
        result.update(smoke_bff(args.image))
    elif args.kind == "gateway":
        result.update(smoke_gateway(args.image))
    print(json.dumps(result, sort_keys=True))
