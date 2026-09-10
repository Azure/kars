#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import argparse
from pathlib import Path


def cleanup():
    work = Path(__file__).resolve().parents[2] / ".e2e-sre-authority"
    # Remove only explicitly owned credential files, never broad namespaces,
    # arbitrary directories, or another process's kubeconfig.
    for name in ("admin.json", "registrar.json", "tenant.json", "normal.json",
                 "old-agent.json", "watch-only.json", "unrelated.json", "opaque.json", "admin-key.pem",
                 "admin-cert.pem", "api-ca.pem", "agent/token", "agent/ca.crt", "agent/namespace"):
        (work / name).unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("phase", choices=("prepare", "legacy", "fresh", "cleanup"))
    options = parser.parse_args()
    if options.phase == "cleanup":
        cleanup()
        return 0
    from sre_authority.common import Harness, SYSTEM
    from sre_authority.fixtures import CONTROL, CONTROL_NS, prepare_legacy
    from sre_authority.migration import fresh_reenrollment, legacy_migration, retire_and_uninstall
    from sre_authority.proxy import proxy_acceptance
    from sre_authority.readiness_diagnostics import collect
    harness = None
    try:
        harness = Harness(options.phase)
        if options.phase == "prepare":
            prepare_legacy(harness)
        elif options.phase == "legacy":
            legacy_migration(harness)
            collect(harness, "ready")
            proxy_acceptance(harness)
            retire_and_uninstall(harness)
            source = harness.get("karssandbox", CONTROL, SYSTEM)
            if source:
                harness.api("DELETE", f"/apis/kars.azure.com/v1alpha1/namespaces/{SYSTEM}/karssandboxes/{CONTROL}",
                    body={"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {
                        "uid": harness.state["control_source_uid"],
                        "resourceVersion": source["metadata"]["resourceVersion"]}}, status=(200, 202))
                harness.poll("owned control fixture cleanup", lambda: harness.get("namespace", CONTROL_NS) is None, seconds=120)
        else:
            fresh_reenrollment(harness)
            collect(harness, "ready")
            proxy_acceptance(harness)
            retire_and_uninstall(harness)
        harness.save()
        return 0
    except Exception as error:
        # Report only the exception class and controlled assertions. API bodies,
        # command output, JWTs and TLS private keys never become failure logs.
        print(f"SRE-FAIL {options.phase}: {str(error) if isinstance(error, AssertionError) else type(error).__name__}", flush=True)
        if harness:
            harness.diagnostics()
        return 1
    finally:
        if harness:
            harness.close()


if __name__ == "__main__":
    raise SystemExit(main())
