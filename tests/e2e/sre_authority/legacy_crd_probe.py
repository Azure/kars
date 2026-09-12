# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Same-Kind historical Helm wait/upgrade proof without controller execution."""

from pathlib import Path
import re
import time
import types

from sre_authority.bootstrap_probe import converted_objects
from sre_authority.common import CONTEXT, Harness, SYSTEM, require
from sre_authority.canonical_seed import dry_run_seed_data
from sre_authority.fixtures import LEGACY_COMMIT, install_historical_chart
from sre_authority.registration_schema import (
    CRD_NAME, CRD_PATH, create_registration_crd, kind_proxy, request, write_report,
)


def preview_registration_identity(h, obj):
    path = f"{CRD_PATH}/{CRD_NAME}"
    absent = h.api("GET", path, status=404).json()
    require(absent.get("kind") == "Status" and absent.get("reason") == "NotFound",
            "New registration preview requires actual absence; no adoption is permitted")
    response = h.api("POST", CRD_PATH + "?dryRun=All&fieldManager=helm&fieldValidation=Strict", body=obj)
    body = response.json()
    metadata = body.get("metadata") if isinstance(body, dict) else None
    metadata = metadata if isinstance(metadata, dict) else {}
    valid = (response.status_code == 201 and isinstance(body, dict)
             and body.get("kind") == "CustomResourceDefinition" and metadata.get("name") == CRD_NAME
             and isinstance(metadata.get("uid"), str) and bool(metadata["uid"])
             and metadata.get("resourceVersion", "") == "")
    write_report(h.root, "migration-seed-crd-preview.json", {
        "resource": CRD_NAME, "httpStatus": response.status_code,
        "category": "accepted" if valid else "unexpected-preview-identity",
        "uidPresent": isinstance(metadata.get("uid"), str) and bool(metadata["uid"]),
        "resourceVersionPresent": "resourceVersion" in metadata,
    })
    require(valid, "New registration server CREATE preview did not have its exact non-persisted identity shape")
    after = h.api("GET", path, status=404).json()
    require(after.get("kind") == "Status" and after.get("reason") == "NotFound",
            "Server CREATE preview persisted a CRD unexpectedly")
    require("uid" not in obj["metadata"] and "resourceVersion" not in obj["metadata"],
            "An ephemeral preview identity entered the real CREATE request")


def exercise(root):
    h = Harness.__new__(Harness)
    h.root, h.work = root, root / ".e2e-legacy-helm"
    h.work.mkdir(mode=0o700)
    h.deadline, h.phase = time.monotonic() + 300, "legacy-helm-proof"
    with kind_proxy(root) as (port, version):
        require(re.fullmatch(r"v1\.31\.\d+(?:[-+].*)?", version.get("gitVersion", "")) is not None,
                "Historical seed API proof requires the pinned Kubernetes 1.31 server")
        def api(method, path, *, body=None, status=None):
            code, obj = request(port, method, path, body)
            if status is not None:
                require(code in (status if isinstance(status, tuple) else (status,)),
                        f"Historical Helm API proof: HTTP {code}")
            return types.SimpleNamespace(status_code=code, json=lambda: obj)
        h.api = api
        install_historical_chart(h)
        dry_run_seed_data(h)
        rendered = h.run(["helm", "template", "kars", str(root / "deploy/helm/kars"),
                         "--namespace", SYSTEM, "--show-only", "templates/crd-karssreregistration.yaml"])
        objects = converted_objects(h.k("create", "--dry-run=client", "--validate=strict",
                                        "-f", "-", "-o", "json", data=rendered))
        require(len(objects) == 1, "Historical Helm proof requires one registration CRD")
        obj = objects[0]
        obj["metadata"].setdefault("labels", {})["app.kubernetes.io/managed-by"] = "Helm"
        obj["metadata"]["annotations"] = {
            "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": SYSTEM}
        preview_registration_identity(h, obj)
        create_registration_crd(h, obj)
        h.k("wait", "--for=condition=Established", "crd/karssreregistrations.kars.azure.com",
            "--timeout=60s", timeout=70)
        h.run(["helm", "--kube-context", CONTEXT, "upgrade", "kars", str(root / "deploy/helm/kars"),
               "--namespace", SYSTEM, "--reset-then-reuse-values", "--set", "sre.authorityStage=true",
               "--dry-run=server"], timeout=90)
        write_report(root, "legacy-helm-readiness.json", {
            "apiServer": version, "legacyCommit": LEGACY_COMMIT,
            "historicalInstallAndPostInstallHook": "passed", "currentAuthorityServerDryRun": "passed",
            "historicalSeedStrictServerDryRuns": 5, "historicalSeedPersistence": "unchanged",
            "historicalNestedParamsRejection": "passed",
            "newCrdPreviewWithoutPersistedRevision": "passed",
            "controllerReplicas": 0, "legacyCRDs": 18, "crdCreation": "native-Helm-only"})


if __name__ == "__main__":
    try:
        exercise(Path(__file__).resolve().parents[3])
    except Exception as error:
        detail = str(error) if isinstance(error, AssertionError) else type(error).__name__
        print(f"SRE-LEGACY-HELM-FAIL {detail}", flush=True)
        raise SystemExit(1) from None
