# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Package unchanged upstream modules and hash-locked wheels using AL3 Python."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile

if sys.version_info[:2] != (3, 12):
    raise RuntimeError("The sidecar requires the AL3 Python 3.12 builder")
build = Path(sys.argv[1]).resolve()
archive = build / "source.tar.gz"
if hashlib.sha256(archive.read_bytes()).hexdigest() != "cc71d13773d6ea86ed5c9f1ccae7c2fd5c3a2febe5330e7bc663b44b9adc4bc9":
    raise RuntimeError("Sidecar source checksum mismatch")
source = build / "source"
source.mkdir()
with tarfile.open(archive) as package:
    package.extractall(source, filter="data")
upstream = source / "k8s-sidecar-070c102fac9bce0eeda44af52d87b6acdfd00f0c"
runtime = build / "sidecar"
env = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1", "PIP_DISABLE_PIP_VERSION_CHECK": "1"}
subprocess.run([
    sys.executable, "-m", "pip", "install", "--no-cache-dir", "--no-compile",
    "--require-hashes", "--only-binary=:all:", "--target", str(runtime / "site-packages"),
    "--requirement", str(build / "requirements.lock"),
], env=env, check=True)
(runtime / "src").mkdir()
for module in (upstream / "src").glob("*.py"):
    shutil.copy2(module, runtime / "src" / module.name)
env["PYTHONPATH"] = str(runtime / "src") + os.pathsep + str(runtime / "site-packages")
subprocess.run([sys.executable, "-m", "pip", "check"], env=env, check=True)
subprocess.run([
    sys.executable, "-c",
    "import sidecar, client, healthz, helpers, logger, resources; "
    "import kubernetes, requests, yaml; assert callable(sidecar.main); "
    "print('SIDECAR_IMPORTS_PASSED')",
], env=env, check=True)
licenses = build / "licenses"
licenses.mkdir(mode=0o755)
licenses.chmod(0o755)
shutil.copy2(upstream / "LICENSE", licenses / "LICENSE")
(licenses / "LICENSE").chmod(0o444)
for item in (runtime, *runtime.rglob("*")):
    item.chmod(0o755 if item.is_dir() else 0o444)
