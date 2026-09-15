# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Build-time, checksum-pinned upstream client; never installs on the host."""
import hashlib
import io
import json
import os
from pathlib import Path
import tarfile
import re
import sys
import urllib.request

CHECKSUMS = {
    "amd64": "701d9e118e01dc0e5447aa2342460e8f5ac5fda0ff9ab42ae7656c81dcc81deb",
    "arm64": "2a43de24d41ea32ea8c8be3474fb64d68e0265d3ffe9780dc9c2f81d7aaa3845",
}
arch = os.environ["TARGETARCH"]
if arch not in CHECKSUMS:
    raise SystemExit("only Linux amd64 and arm64 clients are supported")
revision = os.environ["SOURCE_REVISION"]
base = os.environ["PYTHON_BASE"]
if sys.version_info < (3, 12) or not re.fullmatch(r"[0-9a-f]{40}", revision):
    raise SystemExit("Python 3.12+ and a full public source revision are required")
if not re.fullmatch(r".+@sha256:[0-9a-f]{64}", base):
    raise SystemExit("PYTHON_BASE must use a reviewed image digest")
expected = CHECKSUMS[arch]
url = f"https://github.com/inspektor-gadget/inspektor-gadget/releases/download/v0.53.2/kubectl-gadget-linux-{arch}-v0.53.2.tar.gz"
with urllib.request.urlopen(url, timeout=60) as response:
    data = response.read()
if hashlib.sha256(data).hexdigest() != expected:
    raise SystemExit("upstream client checksum mismatch")
with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
    member = archive.getmember("kubectl-gadget")
    if not member.isfile():
        raise SystemExit("upstream client is not a regular file")
    with archive.extractfile(member) as source:
        binary = Path("/usr/local/bin/kubectl-gadget")
        binary.write_bytes(source.read())
        binary.chmod(0o755)
Path("/tmp/build.json").write_text(json.dumps({
    "source": "https://github.com/Azure/kars", "source_revision": revision,
    "python_base": base, "ig_version": "v0.53.2", "architecture": arch,
    "client_archive_sha256": expected,
}))
