"""Real operator updates must preserve other workspaces, not just render valid grants."""

import copy

from enrollment import enroll
from native_api import BRIDGE, CORE, require, resource, uid

PREFIX = "kars.azure.com/private-"


def scope_snapshot(setup, namespaces):
    result = {}
    for namespace in namespaces:
        value = setup.admin.get(f"/api/v1/namespaces/{namespace}")
        result[namespace] = {
            "uid": uid(value),
            "private": {key: value for key, value in value["metadata"].get("annotations", {}).items()
                        if key.startswith(PREFIX)},
        }
        require(result[namespace]["private"].get(PREFIX + "state") == "Qualified",
                "Continuity requires a genuinely qualified namespace")
    return result


def writer_allowed(actor, namespace):
    result = actor.create("/apis/authorization.k8s.io/v1/selfsubjectaccessreviews", {
        "apiVersion": "authorization.k8s.io/v1", "kind": "SelfSubjectAccessReview",
        "spec": {"resourceAttributes": {
            "namespace": namespace, "group": "kars.azure.com",
            "resource": "karscredentialgrants", "name": "workspace",
            "verb": "use-agent-credentials",
        }},
    })
    require(result.get("status", {}).get("allowed") is True
            and not result.get("status", {}).get("evaluationError"),
            "Existing writer lost its native grant capability")


def run(credentials):
    setup = credentials.setup
    existing_namespace = "native-grant-preflight"
    original = setup.ready_grant(existing_namespace)
    scopes = [CORE, BRIDGE, existing_namespace]
    initial = scope_snapshot(setup, scopes)
    writer_allowed(credentials.actor, existing_namespace)
    original = enroll(
        setup, existing_namespace, credentials.writer,
        [*original["spec"]["agentKeys"], "DISCORD_BOT_TOKEN"], previous=original,
    )
    require(scope_snapshot(setup, scopes) == initial,
            "Updating the sole active grant changed its private qualification")
    writer_allowed(credentials.actor, existing_namespace)
    original_spec = copy.deepcopy(original["spec"])

    namespace = "native-grant-update"
    credentials.workspace(namespace)
    require(scope_snapshot(setup, scopes) == initial,
            "Adding a workspace changed existing private qualification")
    require(uid(setup.ready_grant(existing_namespace)) == uid(original),
            "Adding a workspace replaced the earlier grant")
    reviewed = setup.ready_grant(namespace)
    scopes.append(namespace)
    before_update = scope_snapshot(setup, scopes)
    writer_allowed(credentials.actor, namespace)
    keys = [*reviewed["spec"]["agentKeys"], "DISCORD_BOT_TOKEN"]
    updated = enroll(setup, namespace, credentials.writer, keys, previous=reviewed)
    require(uid(updated) == uid(reviewed) and updated["spec"]["agentKeys"] == keys,
            "Reviewed active-grant update did not preserve its identity and key intent")
    require(scope_snapshot(setup, scopes) == before_update,
            "An ordinary key update changed shared private receipts or epochs")
    other = setup.ready_grant(existing_namespace)
    require(uid(other) == uid(original) and other["spec"] == original_spec,
            "An ordinary key update changed another workspace grant")
    for name in (existing_namespace, namespace):
        writer_allowed(credentials.actor, name)
        credentials.actor.request("GET", resource(name, "secrets", group="/api/v1"), expected=(403,))
