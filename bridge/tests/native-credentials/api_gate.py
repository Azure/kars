"""Real API prerequisite, deliberately independent of builds and SRE migration.

There are no runtime credentials in this lane. Schema/admission failures are
not bypassed, and this lane makes no claim about NetworkPolicy enforcement.
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import time

from source_revision import CORE_REVISION
from schema_preparation import prepare_schemas

ROOT = Path(__file__).resolve().parents[2]
EVIDENCE = ROOT / ".native/evidence/api.json"


def run(*args, check=True):
    result = subprocess.run(
        args, cwd=ROOT, text=True, capture_output=True, timeout=180, check=False
    )
    if check and result.returncode:
        # Only called before credential/runtime setup. Never use this helper to
        # print a general Kubernetes resource, pod log, or BFF response body.
        raise RuntimeError(f"{args[0]} failed: {result.stderr[-16000:]}")
    return result


def kubernetes(*args):
    return json.loads(run("kubectl", *args, "-o", "json").stdout)


def main():
    evidence = {
        "coreRevision": CORE_REVISION,
        "bridgeRevision": run("git", "rev-parse", "HEAD").stdout.strip(),
        "lane": "no-active-sre-api-prerequisite",
        "runtimeQualified": False,
        "networkPolicyEnforcementQualified": False,
        "activeSreCombinedQualified": False,
        "markers": [],
    }
    try:
        actual = run("git", "-C", ".native/core", "rev-parse", "HEAD").stdout.strip()
        if actual != CORE_REVISION or os.environ.get("CORE_REVISION") != CORE_REVISION:
            raise RuntimeError("Exact public core checkout mismatch")
        # The chart includes its namespace. Prepare its Helm ownership before
        # install so release storage and the Namespace template do not race.
        run("kubectl", "create", "namespace", "kars-system")
        run("kubectl", "label", "namespace", "kars-system", "app.kubernetes.io/managed-by=Helm")
        run("kubectl", "annotate", "namespace", "kars-system",
            "meta.helm.sh/release-name=kars", "meta.helm.sh/release-namespace=kars-system")
        evidence["schemaPreparation"] = prepare_schemas(
            "tests/native-credentials/api-values.yaml", "kind-bridge-native-api")
        evidence["markers"].append("native-schemas-published-before-admission")
        run(
            "helm", "install", "kars", ".native/core/deploy/helm/kars",
            "--namespace", "kars-system",
            "--values", "tests/native-credentials/api-values.yaml",
            "--timeout", "120s", "--kube-context", "kind-bridge-native-api",
        )
        evidence["markers"].append("native-chart-install")
        crds = kubernetes("get", "customresourcedefinitions")["items"]
        required = {
            "karscredentialgrants.kars.azure.com",
            "karssandboxes.kars.azure.com",
            "karstasks.kars.azure.com",
            "karsteams.kars.azure.com",
        }
        present = {item["metadata"]["name"] for item in crds}
        if not required <= present:
            raise RuntimeError("Required credential/consumer CRDs are absent")
        run("kubectl", "wait", "--for=condition=Established", "--timeout=60s",
            *[f"crd/{name}" for name in sorted(required)])
        evidence["markers"].append("native-credential-crds-established")
        deadline = time.monotonic() + 60
        while True:
            policies = [
                item for item in kubernetes("get", "validatingadmissionpolicies")["items"]
                if item["metadata"]["name"].startswith("kars-")
            ]
            if not policies:
                raise RuntimeError("Core admission policies are absent")
            pending = [
                item["metadata"]["name"] for item in policies
                if item.get("status", {}).get("observedGeneration")
                != item["metadata"]["generation"]
            ]
            if not pending:
                break
            if time.monotonic() >= deadline:
                raise RuntimeError("Admission type-checking did not acknowledge current generations")
            time.sleep(1)
        warnings = [
            {"policy": item["metadata"]["name"], "field": warning.get("fieldRef"),
             "warning": warning.get("warning")}
            for item in policies
            for warning in item.get("status", {}).get("typeChecking", {}).get("expressionWarnings", [])
        ]
        evidence["admissionWarnings"] = warnings
        if warnings:
            raise RuntimeError("Native admission expression warnings; see secret-free evidence")
        evidence["markers"].append("native-admission-typechecking-clean")
        registrations = kubernetes("get", "karssreregistrations", "--all-namespaces")["items"]
        if registrations:
            raise RuntimeError("No-active-SRE lane unexpectedly contains registration")
        evidence["markers"].append("no-active-sre-registration")
        from admission_cases import run_cases
        evidence["nativeAdmissionCases"] = run_cases()
        if evidence["nativeAdmissionCases"]["result"] != "passed":
            raise RuntimeError("Native admission positive/negative cases failed; see bounded evidence")
        evidence["markers"].append("native-admission-positive-and-intended-denial-cases")
        evidence["result"] = "passed"
    except Exception as error:
        evidence["result"] = "failed"
        from native_api import Failure
        evidence["failure"] = str(error) if isinstance(error, (RuntimeError, Failure)) else type(error).__name__
        print(evidence["failure"], file=sys.stderr)
    finally:
        EVIDENCE.parent.mkdir(parents=True, exist_ok=True)
        EVIDENCE.write_text(json.dumps(evidence, indent=2) + "\n")
        print(json.dumps({key: evidence[key] for key in ("coreRevision", "lane", "markers", "result")}))
    return 0 if evidence["result"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
