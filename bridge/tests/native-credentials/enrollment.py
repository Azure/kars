"""Exercise the shipped operator preview/apply path, never fabricated activation."""

import json

from native_api import BRIDGE, CORE, ROOT, WRITER, Failure, core, private_file, require, resource, uid, until
from operator_diagnostics import operator_command

CLI = ROOT / ".native/core/cli/dist/index.js"


def enroll(setup, namespace, writer, keys):
    require(CLI.is_file(), "The exact core CLI must be built before native enrollment")
    path = resource(namespace, "karscredentialgrants", "workspace")
    require(setup.admin.optional(path) is None, "Native enrollment refuses an existing grant")
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
        and reviewed.get("metadata") == {"name": "workspace", "namespace": namespace},
        "Operator preview did not describe one exclusive native grant",
    )
    spec = reviewed.get("spec", {})
    expected_writer = {"namespace": BRIDGE, "name": WRITER, "uid": uid(writer)}
    require(
        isinstance(spec, dict)
        and spec.get("workspaceUid") == uid(workspace)
        and spec.get("writers") == [expected_writer]
        and spec.get("agentKeys") == keys
        and isinstance(spec.get("privateActivation"), dict)
        and spec.get("privateActivation", {}).get("phase") == "reviewed",
        "Operator preview did not bind the requested native identities and keys",
    )
    review_file = private_file(f"grant-review-{namespace}.json", json.dumps(reviewed))
    operator_command("apply", "node", str(CLI), "credentials", "grant", "apply", str(review_file), timeout=360)
    grant = setup.ready_grant(namespace)
    recorded = grant.get("spec", {})
    activation = recorded.get("privateActivation") if isinstance(recorded, dict) else None
    require(
        isinstance(recorded, dict)
        and recorded.get("workspaceUid") == uid(workspace)
        and recorded.get("writers") == [expected_writer]
        and recorded.get("agentKeys") == keys
        and isinstance(activation, dict)
        and activation.get("phase") == "qualified",
        "The recorded native grant differs from its operator review",
    )
    return grant
