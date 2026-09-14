# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""CI-only acquisition of pinned Kind and the real E2E metrics manifest.

Curl gets at most three attempts per asset, 10s to connect and 30s per attempt,
within a shared caller deadline. Only transient transport failures, 429 and 5xx
retry (1s/2s backoff). HTTP bodies and subprocess errors are never diagnostics.
Kind uses its official release checksum, not a cache or an unchecked fallback.
Metrics retains its existing HTTPS provenance; no new digest is asserted.
Its caller shares the original 90s fetch/apply budget.
"""

import argparse
import hashlib
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
from time import monotonic, sleep
import uuid

KIND_VERSION = "v0.24.0"
KIND_RELEASE = f"https://github.com/kubernetes-sigs/kind/releases/download/{KIND_VERSION}"
METRICS_URL = (
    "https://github.com/kubernetes-sigs/metrics-server/releases/download/v0.7.2/components.yaml"
)
ATTEMPTS = 3
CONNECT_SECONDS = 10
ATTEMPT_SECONDS = 30
KIND_SECONDS = 120
TRANSIENT_CURL = frozenset((5, 6, 7, 16, 18, 28, 52, 55, 56, 92))


class AcquisitionError(Exception):
    """The message is a fixed category, never external error text."""


def remaining(deadline):
    value = deadline - monotonic()
    if value <= 0:
        raise AcquisitionError("deadline")
    return value


def download(url, destination, deadline, max_bytes):
    destination = Path(destination)
    partial = destination.with_name(destination.name + ".part")
    if destination.exists():
        raise AcquisitionError("local-io")
    # All callers supply a unique, private directory. Exclusivity prevents
    # adopting another invocation's partial file or exposing it as a result.
    descriptor = os.open(partial, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    os.close(descriptor)
    try:
        for attempt in range(ATTEMPTS):
            budget = remaining(deadline)
            timeout = min(ATTEMPT_SECONDS, budget)
            try:
                result = subprocess.run(
                    ["curl", "--disable", "--proto", "=https", "--proto-redir", "=https",
                     "--location", "--max-redirs", "5", "--fail", "--silent", "--show-error",
                     "--connect-timeout", str(min(CONNECT_SECONDS, timeout)),
                     "--max-time", str(timeout), "--retry", "0",
                     "--max-filesize", str(max_bytes), "--output", str(partial),
                     "--write-out", "%{http_code}", url],
                    capture_output=True, timeout=budget,
                )
            except subprocess.TimeoutExpired:
                raise AcquisitionError("deadline") from None
            except OSError:
                raise AcquisitionError("local-io") from None
            status = int(result.stdout) if re.fullmatch(rb"[1-5][0-9]{2}", result.stdout) else None
            retry = False
            if status is not None and not 200 <= status < 300:
                category = f"http-{status}"
                retry = (status == 429 or status >= 500) and result.returncode in (0, 22)
            elif result.returncode:
                category = "timeout" if result.returncode == 28 else "transport"
                retry = result.returncode in TRANSIENT_CURL
            elif status is None:
                category = "invalid-status"
            elif not 0 < partial.stat().st_size <= max_bytes:
                category = "invalid-size"
            else:
                remaining(deadline)
                partial.rename(destination)
                return
            if not retry or attempt == ATTEMPTS - 1:
                raise AcquisitionError(category)
            delay = attempt + 1
            if remaining(deadline) <= delay:
                raise AcquisitionError("deadline")
            # Do not retain or append an error body/partial transfer on retry.
            partial.write_bytes(b"")
            sleep(delay)
    finally:
        partial.unlink(missing_ok=True)


def checksum_for(data, filename):
    match = re.fullmatch(rb"([0-9a-fA-F]{64}) [ *]" + re.escape(filename.encode("ascii")) + rb"\n?", data)
    if not match:
        raise AcquisitionError("checksum-format")
    return match[1].decode("ascii").lower()


def install_kind(work, github_path):
    if platform.system() != "Linux":
        raise AcquisitionError("unsupported-platform")
    arch = {"x86_64": "amd64", "aarch64": "arm64"}.get(platform.machine())
    if not arch:
        raise AcquisitionError("unsupported-platform")
    filename = f"kind-linux-{arch}"
    directory = Path(work).resolve() / (".ci-kind-" + uuid.uuid4().hex)
    directory.mkdir(mode=0o700)
    checksum = directory / (filename + ".sha256sum")
    binary = directory / "kind"
    published = False
    deadline = monotonic() + KIND_SECONDS
    try:
        download(f"{KIND_RELEASE}/{filename}.sha256sum", checksum, deadline, 1024)
        expected = checksum_for(checksum.read_bytes(), filename)
        download(f"{KIND_RELEASE}/{filename}", binary, deadline, 50 * 1024 * 1024)
        digest = hashlib.sha256()
        with binary.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        if digest.hexdigest() != expected:
            raise AcquisitionError("checksum-mismatch")
        remaining(deadline)
        binary.chmod(0o700)
        # No binary execution, chmod, PATH publication or cache reuse precedes
        # the exact-filename checksum and digest checks.
        with Path(github_path).open("a", encoding="utf-8") as output:
            output.write(str(directory) + "\n")
        published = True
        return binary
    finally:
        checksum.unlink(missing_ok=True)
        if not published:
            binary.unlink(missing_ok=True)
            directory.rmdir()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="asset", required=True)
    subparsers.add_parser("kind")
    metrics = subparsers.add_parser("metrics")
    metrics.add_argument("--destination", type=Path, required=True)
    metrics.add_argument("--budget", type=float, required=True)
    args = parser.parse_args()
    try:
        if args.asset == "kind":
            github_path = os.environ.get("GITHUB_PATH")
            if os.environ.get("GITHUB_ACTIONS") != "true" or not github_path:
                raise AcquisitionError("ci-environment")
            install_kind(Path.cwd(), github_path)
        else:
            if not 0 < args.budget <= 90:
                raise AcquisitionError("deadline")
            download(METRICS_URL, args.destination, monotonic() + args.budget, 1024 * 1024)
    except AcquisitionError as error:
        print(f"CI-ACQUISITION-FAILURE {error}", file=sys.stderr)
        return 1
    except OSError:
        print("CI-ACQUISITION-FAILURE local-io", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
