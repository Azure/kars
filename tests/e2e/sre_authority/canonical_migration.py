# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Native fixtures for the public CLI's closed BASE365 schema migration.

No migration logic is duplicated here. All qualification/writes run through
`authority stage`; this module only creates disposable data and checks evidence.
"""

import copy
import hashlib
import json

from .common import SYSTEM, require
from .canonical_seed import dry_run_seed_data, prove_nested_params_support, request_seed, seed_definitions
from .task_schema_conflicts import task_schema_conflict

CRDS = "/apis/apiextensions.k8s.io/v1/customresourcedefinitions"
STAGE = ("authority", "stage", "--controller-image", "kars-controller:e2e",
         "--router-image", "kars-inference-router:e2e")


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def data_snapshot(obj):
    meta = obj["metadata"]
    return {
        "uid": meta["uid"], "resourceVersion": meta["resourceVersion"],
        "specDigest": digest(obj.get("spec")),
        "statusDigest": digest(obj.get("status")),
    }


def seed_data(h):
    controller = h.get("deployment", "kars-controller", SYSTEM)
    require(controller["spec"].get("replicas") == 0,
            "Migration fixture must keep the real controller paused while measuring data")
    dry_run_seed_data(h)
    fixtures = []
    for resource, obj in seed_definitions():
        created = request_seed(h, resource, obj, dry_run=False)
        fixtures.append({"resource": resource, "name": created["metadata"]["name"], "before": data_snapshot(created)})
    h.passed("Native canonical migration fixture data created without launching workloads")
    return fixtures


def assert_data_unchanged(h, fixtures):
    for fixture in fixtures:
        obj = h.get(fixture["resource"], fixture["name"], SYSTEM)
        require(obj and data_snapshot(obj) == fixture["before"],
                "Canonical migration changed custom-resource data or its UID/resourceVersion")


def deny_late_conflicts(h, fixtures):
    """A final CRD conflict must prevent even the earlier action conversion."""
    action = h.get("crd", "karssreactions.kars.azure.com")
    action_before = {"uid": action["metadata"]["uid"], "spec": copy.deepcopy(action["spec"])}
    binding = h.get("clusterrolebinding", "kars-sre-reader")
    subjects = copy.deepcopy(binding["subjects"])
    for fault in ("owner", "schema"):
        with task_schema_conflict(h, fault):
            for mode, flags in (("preview", ("--dry-run",)), ("apply", ())):
                rejected = h.cli(*STAGE, *flags, expected=None, timeout=180)
                require(rejected.returncode != 0, "A foreign/custom schema unexpectedly qualified")
                actual = h.get("crd", "karssreactions.kars.azure.com")
                require(actual["metadata"]["uid"] == action_before["uid"] and actual["spec"] == action_before["spec"],
                        "Late migration conflict changed the action schema")
                require(h.get("clusterrolebinding", "kars-sre-reader")["subjects"] == subjects,
                        "Migration preflight changed an existing subject")
                assert_data_unchanged(h, fixtures)
                h.passed(f"Native canonical migration {fault} conflict refused during {mode} before any action/schema conversion")


def finish_data_proof(h, fixtures):
    assert_data_unchanged(h, fixtures)
    prove_nested_params_support(h)
    assert_data_unchanged(h, fixtures)
    h.passed("Native BASE365-to-current schema migration preserved all fixture data/UIDs/resourceVersions")
    for fixture in fixtures:
        obj = h.get(fixture["resource"], fixture["name"], SYSTEM)
        plural = {
            "karstask": "karstasks", "karsteam": "karsteams", "mcpserver": "mcpservers",
            "karseval": "karsevals", "karssreaction": "karssreactions",
        }[fixture["resource"]]
        h.api("DELETE", f"/apis/kars.azure.com/v1alpha1/namespaces/{SYSTEM}/{plural}/{fixture['name']}",
              body={"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {
                  "uid": obj["metadata"]["uid"], "resourceVersion": obj["metadata"]["resourceVersion"]}},
              status=(200, 202))
