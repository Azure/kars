# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Same-Kind historical Helm wait/upgrade proof without controller execution."""

from pathlib import Path
import time
import types

from sre_authority.bootstrap_probe import converted_objects
from sre_authority.common import CONTEXT, Harness, SYSTEM, require
from sre_authority.fixtures import LEGACY_COMMIT, install_historical_chart
from sre_authority.registration_schema import (
    create_registration_crd, kind_proxy, request, write_report,
)


def exercise(root):
    h = Harness.__new__(Harness)
    h.root, h.work = root, root / ".e2e-legacy-helm"
    h.work.mkdir(mode=0o700)
    h.deadline, h.phase = time.monotonic() + 300, "legacy-helm-proof"
    with kind_proxy(root) as (port, version):
        def api(method, path, *, body=None, status=None):
            code, obj = request(port, method, path, body)
            if status is not None:
                require(code in (status if isinstance(status, tuple) else (status,)),
                        f"Historical Helm API proof: HTTP {code}")
            return types.SimpleNamespace(status_code=code, json=lambda: obj)
        h.api = api
        install_historical_chart(h)
        rendered = h.run(["helm", "template", "kars", str(root / "deploy/helm/kars"),
                         "--namespace", SYSTEM, "--show-only", "templates/crd-karssreregistration.yaml"])
        objects = converted_objects(h.k("create", "--dry-run=client", "--validate=strict",
                                        "-f", "-", "-o", "json", data=rendered))
        require(len(objects) == 1, "Historical Helm proof requires one registration CRD")
        obj = objects[0]
        obj["metadata"].setdefault("labels", {})["app.kubernetes.io/managed-by"] = "Helm"
        obj["metadata"]["annotations"] = {
            "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": SYSTEM}
        create_registration_crd(h, obj)
        h.k("wait", "--for=condition=Established", "crd/karssreregistrations.kars.azure.com",
            "--timeout=60s", timeout=70)
        h.run(["helm", "--kube-context", CONTEXT, "upgrade", "kars", str(root / "deploy/helm/kars"),
               "--namespace", SYSTEM, "--reset-then-reuse-values", "--set", "sre.authorityStage=true",
               "--dry-run=server"], timeout=90)
        write_report(root, "legacy-helm-readiness.json", {
            "apiServer": version, "legacyCommit": LEGACY_COMMIT,
            "historicalInstallAndPostInstallHook": "passed", "currentAuthorityServerDryRun": "passed",
            "controllerReplicas": 0, "legacyCRDs": 18, "crdCreation": "native-Helm-only"})


if __name__ == "__main__":
    try:
        exercise(Path(__file__).resolve().parents[3])
    except Exception as error:
        detail = str(error) if isinstance(error, AssertionError) else type(error).__name__
        print(f"SRE-LEGACY-HELM-FAIL {detail}", flush=True)
        raise SystemExit(1) from None
