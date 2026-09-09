# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Read-only validation of CRDs from the immutable historical Helm fixture."""

from .bootstrap_probe import converted_objects
from .common import SYSTEM, require
from .registration_schema import CRD_NAME, CRD_PATH

# This is the complete historical inventory, not the current chart's CRDs.
CRDS = {
    "crd-a2aagent.yaml": ("a2aagents", "A2AAgent", "Namespaced"),
    "crd-egressapproval.yaml": ("egressapprovals", "EgressApproval", "Namespaced"),
    "crd-inferencepolicy.yaml": ("inferencepolicies", "InferencePolicy", "Namespaced"),
    "crd-karsapproval.yaml": ("karsapprovals", "KarsApproval", "Namespaced"),
    "crd-karsauthconfig.yaml": ("karsauthconfigs", "KarsAuthConfig", "Cluster"),
    "crd-karseval.yaml": ("karsevals", "KarsEval", "Namespaced"),
    "crd-karsmemory.yaml": ("karsmemories", "KarsMemory", "Namespaced"),
    "crd-karsprofile.yaml": ("karsprofiles", "KarsProfile", "Namespaced"),
    "crd-karsreceipt.yaml": ("karsreceipts", "KarsReceipt", "Namespaced"),
    "crd-karsskill.yaml": ("karsskills", "KarsSkill", "Namespaced"),
    "crd-karssreaction.yaml": ("karssreactions", "KarsSREAction", "Namespaced"),
    "crd-karstask.yaml": ("karstasks", "KarsTask", "Namespaced"),
    "crd-karsteam.yaml": ("karsteams", "KarsTeam", "Namespaced"),
    "crd-mcpserver.yaml": ("mcpservers", "McpServer", "Namespaced"),
    "crd-toolpolicy.yaml": ("toolpolicies", "ToolPolicy", "Namespaced"),
    "crd-trustgraph.yaml": ("trustgraphs", "TrustGraph", "Cluster"),
    "crd.yaml": ("karssandboxes", "KarsSandbox", "Namespaced"),
}
# The historical crd.yaml contains both KarsSandbox and KarsPairing.
IDENTITIES = (*CRDS.values(), ("karspairings", "KarsPairing", "Namespaced"))


def validate_rendered_crds(rendered, historical):
    require(isinstance(historical, dict) and set(historical) == set(CRDS),
            "Historical CRD inventory differs from the immutable baseline")
    require(isinstance(rendered, list) and len(rendered) == len(IDENTITIES),
            "Historical render omitted or added resources")
    expected = {}
    for filename, identity in CRDS.items():
        identities = [identity, IDENTITIES[-1]] if filename == "crd.yaml" else [identity]
        require(isinstance(historical[filename], list) and len(historical[filename]) == len(identities),
                "Historical template has an unexpected number of CRDs")
        for obj, expected_identity in zip(historical[filename], identities):
            validate_historical_crd(obj, expected_identity)
            expected[obj["metadata"]["name"]] = obj
    seen = set()
    for obj in rendered:
        require(isinstance(obj, dict), "Historical render contained a non-object")
        name = obj.get("metadata", {}).get("name")
        require(name in expected and name not in seen and obj == expected[name],
                "Rendered CRD content differs from its immutable historical template")
        seen.add(name)
    return rendered


def validate_historical_crd(obj, identity):
    plural, kind, scope = identity
    name = f"{plural}.kars.azure.com"
    require(isinstance(obj, dict) and set(obj) == {"apiVersion", "kind", "metadata", "spec"}
            and obj["apiVersion"] == "apiextensions.k8s.io/v1"
            and obj["kind"] == "CustomResourceDefinition"
            and obj["metadata"].get("name") == name
            and not set(obj["metadata"]) - {"name", "labels"}
            and obj["spec"].get("group") == "kars.azure.com"
            and obj["spec"].get("scope") == scope
            and obj["spec"].get("names", {}).get("plural") == plural
            and obj["spec"].get("names", {}).get("kind") == kind,
            "Historical CRD identity or ownership differs from the immutable baseline")
    versions = obj["spec"].get("versions", [])
    require(len(versions) == 1 and versions[0].get("name") == "v1alpha1"
            and versions[0].get("served") is True and versions[0].get("storage") is True
            and versions[0].get("schema", {}).get("openAPIV3Schema", {}).get("type") == "object",
            "Historical CRD version/schema differs from the immutable baseline")


def render_legacy_crds(h, chart, sources):
    require(set(sources) == set(CRDS)
            and {path.name for path in (chart / "templates").glob("crd*.yaml")} == set(CRDS),
            "Historical archive contains an unexpected CRD inventory")
    for filename, content in sources.items():
        require("{{" not in content and (chart / "templates" / filename).read_text() == content,
                "Historical CRD template changed after immutable archive extraction")
    args = ["helm", "template", "kars", str(chart), "--namespace", SYSTEM,
            "--set", "controller.replicas=0", "--set", "sre.enabled=false"]
    for filename in CRDS:
        args += ["--show-only", f"templates/{filename}"]
    rendered = h.run(args, timeout=45)
    def convert(text):
        return converted_objects(h.k("create", "--dry-run=client", "--validate=strict",
                                     "-f", "-", "-o", "json", data=text))
    original = convert("\n---\n".join(sources[filename] for filename in CRDS))
    require(len(original) == len(IDENTITIES), "Immutable historical CRD count changed")
    historical, offset = {}, 0
    for filename in CRDS:
        count = 2 if filename == "crd.yaml" else 1
        historical[filename] = original[offset:offset + count]
        offset += count
    return validate_rendered_crds(convert(rendered), historical)


def preflight_legacy_crds(h, chart, sources):
    objects = render_legacy_crds(h, chart, sources)
    # Only Helm creates these objects, retaining its native SSA field ownership.
    # An existing object, even one claiming this release, is never adopted.
    for name in [CRD_NAME] + [obj["metadata"]["name"] for obj in objects]:
        response = h.api("GET", f"{CRD_PATH}/{name}", status=404)
        body = response.json()
        require(body.get("kind") == "Status" and body.get("reason") == "NotFound",
                "Historical CRD absence was not a Kubernetes NotFound")
    h.passed("All 18 historical CRDs match the immutable archive and are absent before Helm-owned creation")
