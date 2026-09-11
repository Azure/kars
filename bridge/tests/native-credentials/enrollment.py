"""Exercise the shipped operator preview/apply path, never fabricated activation."""

import json

from native_api import BRIDGE, CORE, ROOT, WRITER, Failure, core, private_file, require, resource, uid, until
from operator_diagnostics import operator_command

CLI = ROOT / ".native/core/cli/dist/index.js"


def enroll(setup, namespace, writer, keys, *, previous=None, observations=()):
    require(CLI.is_file(), "The exact core CLI must be built before native enrollment")
    path = resource(namespace, "karscredentialgrants", "workspace")
    existing = setup.admin.optional(path)
    expected_metadata = {"name": "workspace", "namespace": namespace}
    if previous is None:
        require(existing is None, "Native enrollment refuses an existing grant")
    else:
        require(
            existing is not None and uid(existing) == uid(previous)
            and existing["metadata"]["resourceVersion"] == previous["metadata"]["resourceVersion"]
            and existing["spec"] == previous["spec"],
            "Native key update requires the exact previously reviewed grant",
        )
        require(
            previous["spec"].get("enabled") is True
            and all(not previous["spec"].get(field) for field in (
                "integrationStores", "legacyImports", "observationTargets", "githubConnections",
                "controller", "bridgeConsumers",
            )),
            "Native key-update fixture accepts only an agent-key grant",
        )
        expected_metadata.update(
            uid=uid(previous), resourceVersion=previous["metadata"]["resourceVersion"],
        )
    workspace = setup.admin.get(f"/api/v1/namespaces/{namespace}")
    current_writer = setup.admin.get(core(BRIDGE, "serviceaccounts", WRITER))
    require(uid(current_writer) == uid(writer), "Native writer UID changed before operator review")
    definition = json.loads((ROOT / ".native/core/deploy/helm/kars/files/private-consumption.json").read_text())
    for name in definition["controllers"]:
        until(
            f"actual workload-controller account {name}",
            lambda name=name: setup.admin.optional(core("kube-system", "serviceaccounts", name)),
            90,
        )

    args = [
        "node", str(CLI), "credentials", "grant", "preview",
        "--namespace", namespace, "--writer", f"{BRIDGE}/{WRITER}",
        "--private-root", CORE, "--private-controller-profile", "service-accounts",
        "--private-consumer", f"{BRIDGE}/Deployment/kars-bridge-bff",
    ]
    expected_observations = list(observations)
    for target in expected_observations:
        require(
            isinstance(target, dict) and set(target) == {"kind", "namespace", "name", "uid"}
            and target["kind"] == "KarsSandbox" and target["namespace"] == namespace
            and all(isinstance(target[key], str) and target[key] for key in ("name", "uid")),
            "Native observation enrollment requires exact workspace Sandbox identities",
        )
        args.extend(["--observe", target["name"], "--private-consumer",
                     f"kars-{target['name']}/Deployment/{target['name']}"])
    for key in keys:
        args.extend(["--agent-key", key])
    try:
        reviewed = json.loads(operator_command("preview", *args, timeout=180))
    except json.JSONDecodeError:
        raise Failure("Core operator preview did not return a JSON document") from None
    require(
        isinstance(reviewed, dict)
        and reviewed.get("apiVersion") == "kars.azure.com/v1alpha1"
        and reviewed.get("kind") == "KarsCredentialGrant"
        and reviewed.get("metadata") == expected_metadata,
        "Operator preview did not describe the exact native grant incarnation",
    )
    spec = reviewed.get("spec", {})
    expected_writer = {"namespace": BRIDGE, "name": WRITER, "uid": uid(writer)}
    if previous is not None:
        require(previous["spec"].get("writers") == [expected_writer]
                and previous["spec"].get("workspaceUid") == uid(workspace),
                "Native key update cannot change workspace or writer identity")
    require(
        isinstance(spec, dict)
        and spec.get("workspaceUid") == uid(workspace)
        and spec.get("writers") == [expected_writer]
        and spec.get("agentKeys") == keys
        and spec.get("observationTargets", []) == expected_observations
        and isinstance(spec.get("privateActivation"), dict)
        and spec.get("privateActivation", {}).get("phase") == "reviewed",
        "Operator preview did not bind the requested native identities and keys",
    )
    review_file = private_file(f"grant-review-{namespace}.json", json.dumps(reviewed))
    operator_command("apply", "node", str(CLI), "credentials", "grant", "apply", str(review_file), timeout=360)
    grant = setup.ready_grant(namespace)
    if previous is not None:
        require(uid(grant) == uid(previous), "Native key update replaced the grant")
    recorded = grant.get("spec", {})
    activation = recorded.get("privateActivation") if isinstance(recorded, dict) else None
    require(
        isinstance(recorded, dict)
        and recorded.get("workspaceUid") == uid(workspace)
        and recorded.get("writers") == [expected_writer]
        and recorded.get("agentKeys") == keys
        and recorded.get("observationTargets", []) == expected_observations
        and isinstance(activation, dict)
        and activation.get("phase") == "qualified",
        "The recorded native grant differs from its operator review",
    )
    return grant
