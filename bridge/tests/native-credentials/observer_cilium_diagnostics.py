"""Read-only, fixed-field Cilium 1.18.5 witnesses for the observer experiment."""

import copy
import json
import re
import subprocess

from native_api import ROOT, Failure, core, require, resource
from observation_diagnostics import READ_ERRORS, identity, owner
import observer_network_diagnostics as network

CILIUM = "/apis/cilium.io/v2"
CONFIG = core("kube-system", "configmaps", "cilium-config")
DAEMONSET = resource("kube-system", "daemonsets", "cilium", "/apis/apps/v1")
CONFIG_OUTPUT = (
    r'jsonpath={.PolicyCIDRMatchMode}{"\t"}{.EnableCiliumNetworkPolicy}{"\t"}'
    r'{.EnableK8sNetworkPolicy}{"\n"}'
)
# v1.18.5 status.go prints a StatusResponse object; this is not an option.Config field.
# https://github.com/cilium/cilium/blob/v1.18.5/api/v1/models/kube_proxy_replacement.go#L43-L45
KUBE_PROXY_OUTPUT = r'jsonpath={.kube-proxy-replacement.mode}{"\n"}'
ENDPOINT_OUTPUT = (
    r'jsonpath={[0].id}{"\t"}{[0].status.identity.id}{"\t"}'
    r'{[0].status.policy.spec.policy-revision}{"\t"}'
    r'{[0].status.policy.realized.policy-revision}{"\t"}'
    r'{[0].status.policy.realized.policy-enabled}{"\t"}'
    r'{[0].status.external-identifiers.k8s-namespace}{"\t"}'
    r'{[0].status.external-identifiers.k8s-pod-name}{"\n"}'
)
UNAVAILABLE = "Cilium diagnostic provenance unavailable"
STAGES = frozenset("""
network_snapshot network_validated origin_recheck namespace_read namespace_identity
configmap_read configmap_identity daemonset_read daemonset_identity account_read account_identity
resource_names agent_list_read agent_select agent_read agent_identity agent_layout agent_constraints
agent_selector configmap_fields config_exec config_framing config_fields config_mode config_defaults
kube_proxy_exec kube_proxy_framing kube_proxy_fields
endpoint_read endpoint_identity endpoint_owner endpoint_fields endpoint_addresses endpoint_pins
endpoint_exec endpoint_framing endpoint_projection_fields endpoint_projection_pins endpoint_revision_bounds
endpoint_recheck endpoint_snapshot_match cnp_list_read cnp_inventory cnp_identity cnp_digest
anchor_read anchor_identity cnp_recheck network_recheck complete
""".split())
FACT_KEYS = frozenset("""
httpStatus exitStatus timedOut byteCount lineCount fieldCount objectShape metadataShape
uidPresent resourceVersionPresent namePresent namespacePresent deleting metadataSyntaxValid
uidMatches resourceVersionMatches
namespaceMatches nameMatches configmapMatches daemonsetMatches accountMatches index count
agentSpecShape containersShape statusesShape templateShape ownerCount ownerMatches listIdentityMatches
containerCount statusCount templateContainerCount ready containerIdPresent restartCountValid
serviceAccountMatches imageMatchesTemplate imageExpectedVersion imageHasDigest selectorShape selectorMatches
dataShape modeSyntax cnpFlagSyntax kubernetesFlagSyntax kubeProxySyntax booleanFieldsValid
requiredTokensPresent
statusShape identityShape networkingShape addressingCount podAddressCount nodeAddressPresent
endpointIdValid securityIdValid podUidMatches addressesMatch nodeMatchesPod nodeMatchesAgent
numericFieldsValid endpointIdMatches securityIdMatches policyModeRecognized namespaceFieldMatches
podFieldMatches realizedAheadOfDesired desiredRevisionValid realizedRevisionValid bindingMatches
specShape specsShape managedFieldsShape authorityMatches anchorKind configurationMatches
""".split())
FACT_VALUES = frozenset("""
object array null string number boolean other missing empty true false json-null
json-empty-array json-nodes-array json-other invalid-json go-nil go-no-value
namespace configmap daemonset account agent
status-true status-false
""".split())


def checkpoint(witness, stage, **facts):
    if witness is None:
        return
    require(stage in STAGES and set(facts) <= FACT_KEYS, UNAVAILABLE)
    require(all(value is None or type(value) is bool
                or (type(value) is int and -(2**31) <= value < 2**63)
                or (isinstance(value, str) and value in FACT_VALUES) for value in facts.values()), UNAVAILABLE)
    witness["lastStage"] = stage
    witness.setdefault("checks", {})[stage] = facts


def shape(value):
    if value is None:
        return "null"
    return {dict: "object", list: "array", str: "string", bool: "boolean",
            int: "number", float: "number"}.get(type(value), "other")


def rendering(value):
    known = {"": "empty", "null": "json-null", "[]": "json-empty-array",
             "<nil>": "go-nil", "<no value>": "go-no-value", "true": "true", "false": "false",
             "True": "status-true", "False": "status-false"}
    if not isinstance(value, str):
        return "other"
    if value in known:
        return known[value]
    try:
        decoded = json.loads(value)
        return "json-nodes-array" if decoded == ["nodes"] else "json-other"
    except ValueError:
        return "invalid-json"


def identity_facts(value):
    metadata = value.get("metadata") if isinstance(value, dict) else None
    fields = metadata if isinstance(metadata, dict) else {}
    return {"objectShape": shape(value), "metadataShape": shape(metadata),
            "uidPresent": isinstance(fields.get("uid"), str) and bool(fields["uid"]),
            "resourceVersionPresent": isinstance(fields.get("resourceVersion"), str) and bool(fields["resourceVersion"]),
            "namePresent": isinstance(fields.get("name"), str) and bool(fields["name"]),
            "namespacePresent": isinstance(fields.get("namespace"), str) and bool(fields["namespace"]),
            "deleting": bool(fields.get("deletionTimestamp")),
            "metadataSyntaxValid": all(isinstance(fields.get(key), str)
                and re.fullmatch(r"[A-Za-z0-9_.:-]{1,253}", fields[key]) is not None
                for key in ("name", "uid", "resourceVersion"))}


def api_read(setup, path, witness, stage):
    checkpoint(witness, stage, httpStatus=None)
    code, value = setup.admin.request("GET", path, expected=tuple(range(100, 600)))
    checkpoint(witness, stage, httpStatus=code, objectShape=shape(value))
    require(code == 200, UNAVAILABLE)
    return value


def read_projection(agent, endpoint_id=None, witness=None):
    if endpoint_id is None:
        arguments = ["config", "--read-only", "--output", CONFIG_OUTPUT]
    else:
        require(type(endpoint_id) is int and 0 < endpoint_id <= 65535, UNAVAILABLE)
        arguments = ["endpoint", "get", str(endpoint_id), "--output", ENDPOINT_OUTPUT]
    prefix = "config" if endpoint_id is None else "endpoint"
    return _read_cli_projection(agent, arguments, prefix, witness)


def read_kube_proxy_projection(agent, witness=None):
    # Keep status' default require-k8s-connectivity=true; never mask daemon/API failure.
    arguments = ["status", "--timeout=10s", "--output", KUBE_PROXY_OUTPUT]
    return _read_cli_projection(agent, arguments, "kube_proxy", witness)


def _read_cli_projection(agent, arguments, prefix, witness):
    checkpoint(witness, f"{prefix}_exec", exitStatus=None, timedOut=False)
    try:
        result = subprocess.run(
            ["kubectl", "--context", "kind-bridge-native", "--request-timeout=10s",
             "exec", "-n", "kube-system", agent["metadata"]["name"], "-c", "cilium-agent",
             "--", "cilium-dbg", *arguments],
            cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=12, check=False,
        )
    except subprocess.TimeoutExpired:
        checkpoint(witness, f"{prefix}_exec", exitStatus=None, timedOut=True)
        raise
    checkpoint(witness, f"{prefix}_exec", exitStatus=result.returncode, timedOut=False,
               byteCount=len(result.stdout))
    require(result.returncode == 0 and len(result.stdout) <= 4096, UNAVAILABLE)
    checkpoint(witness, f"{prefix}_framing", byteCount=len(result.stdout))
    lines = result.stdout.decode("utf8").strip("\r\n").splitlines()
    checkpoint(witness, f"{prefix}_framing", byteCount=len(result.stdout), lineCount=len(lines))
    require(len(lines) == 1, UNAVAILABLE)
    return lines[0].split("\t")


def effective_configuration(fields, witness=None):
    checkpoint(witness, "config_fields", fieldCount=len(fields),
               requiredTokensPresent=len(fields) == 3 and all(isinstance(value, str) and bool(value) for value in fields),
               modeSyntax=rendering(fields[0]) if fields else "missing",
               cnpFlagSyntax=rendering(fields[1]) if len(fields) > 1 else "missing",
               kubernetesFlagSyntax=rendering(fields[2]) if len(fields) > 2 else "missing",
               booleanFieldsValid=all(value in ("true", "false") for value in fields[1:]))
    require(len(fields) == 3 and all(value in ("true", "false") for value in fields[1:]), UNAVAILABLE)
    checkpoint(witness, "config_mode", modeSyntax=rendering(fields[0]))
    modes = json.loads(fields[0])
    require(modes is None or (isinstance(modes, list) and modes in ([], ["nodes"])), UNAVAILABLE)
    return {"policyCIDRMatchMode": modes or [], "ciliumNetworkPolicyEnabled": fields[1] == "true",
            "kubernetesNetworkPolicyEnabled": fields[2] == "true"}


def kube_proxy_mode(fields, witness=None):
    checkpoint(witness, "kube_proxy_fields", fieldCount=len(fields),
               requiredTokensPresent=len(fields) == 1 and isinstance(fields[0], str) and bool(fields[0]),
               kubeProxySyntax=rendering(fields[0]) if fields else "missing",
               policyModeRecognized=len(fields) == 1 and fields[0] in ("True", "False"))
    require(len(fields) == 1 and fields[0] in ("True", "False"), UNAVAILABLE)
    return fields[0] == "True"


def configmap_facts(config):
    data = config.get("data")
    require(isinstance(data, dict), UNAVAILABLE)
    mode = data.get("policy-cidr-match-mode", "")
    enforcement = data.get("enable-policy")
    replacement = data.get("kube-proxy-replacement")
    return {"identity": network.metadata(config),
            "policyCIDRMatchMode": [] if mode == "" else ["nodes"] if mode == "nodes" else ["unrecognized"],
            "policyEnforcementMode": enforcement if enforcement in ("default", "always", "never") else "unrecognized",
            "kubeProxyReplacement": replacement if replacement in ("true", "false") else "unrecognized"}


def policy_authority(value):
    metadata = dict(value["metadata"])
    metadata.pop("resourceVersion", None)
    fields = network.bounded_items(metadata.pop("managedFields", []), 64)
    require(all(isinstance(field, dict) for field in fields), UNAVAILABLE)
    metadata["managedFields"] = [field for field in fields if field.get("subresource") != "status"]
    return network.spec_digest({"metadata": metadata, "spec": value.get("spec"), "specs": value.get("specs")})


def policies(setup, namespace, temporary_name, witness=None):
    value = api_read(setup, resource(namespace, "ciliumnetworkpolicies", group=CILIUM),
                     witness, "cnp_list_read")
    items = value.get("items") if isinstance(value, dict) else None
    checkpoint(witness, "cnp_inventory", objectShape=shape(items),
               count=len(items) if isinstance(items, list) else None)
    values = network.bounded_items(
        items, 32)
    result = []
    for index, value in enumerate(values):
        checkpoint(witness, "cnp_identity", index=index, **identity_facts(value))
        require(value["metadata"].get("namespace") == namespace, UNAVAILABLE)
        if value["metadata"]["name"] == temporary_name:
            continue
        network.metadata(value)
        result.append(value)
    return result


def endpoint_binding(endpoint, pod, agent, witness=None):
    checkpoint(witness, "endpoint_identity", **identity_facts(endpoint))
    metadata = network.metadata(endpoint)
    references = endpoint["metadata"].get("ownerReferences", [])
    checkpoint(witness, "endpoint_owner", namespaceMatches=metadata.get("namespace") == pod["metadata"]["namespace"],
               nameMatches=metadata["name"] == pod["metadata"]["name"],
               objectShape=shape(references), ownerCount=len(references) if isinstance(references, list) else None)
    require(metadata["namespace"] == pod["metadata"]["namespace"]
            and metadata["name"] == pod["metadata"]["name"], UNAVAILABLE)
    require(isinstance(references, list), UNAVAILABLE)
    checkpoint(witness, "endpoint_owner", ownerCount=len(references),
               ownerMatches=len(references) == 1 and isinstance(references[0], dict)
               and references[0].get("apiVersion") == "v1" and references[0].get("kind") == "Pod"
               and references[0].get("name") == pod["metadata"]["name"],
               podUidMatches=len(references) == 1 and isinstance(references[0], dict)
               and references[0].get("uid") == identity(pod)[0])
    require(len(references) == 1 and references[0].get("apiVersion") == "v1"
            and references[0].get("kind") == "Pod" and references[0].get("name") == pod["metadata"]["name"]
            and references[0].get("uid") == identity(pod)[0], UNAVAILABLE)
    status = endpoint["status"]
    checkpoint(witness, "endpoint_fields", statusShape=shape(status),
               identityShape=shape(status.get("identity")) if isinstance(status, dict) else "missing")
    endpoint_id, security_id = status.get("id"), status.get("identity", {}).get("id")
    checkpoint(witness, "endpoint_fields",
               endpointIdValid=type(endpoint_id) is int and 0 < endpoint_id <= 65535,
               securityIdValid=type(security_id) is int and 0 < security_id < 2**32)
    require(type(endpoint_id) is int and 0 < endpoint_id <= 65535
            and type(security_id) is int and 0 < security_id < 2**32, UNAVAILABLE)
    networking = status.get("networking")
    checkpoint(witness, "endpoint_addresses", networkingShape=shape(networking),
               addressingCount=len(networking.get("addressing", [])) if isinstance(networking, dict)
               and isinstance(networking.get("addressing", []), list) else None,
               nodeAddressPresent=isinstance(networking, dict) and isinstance(networking.get("node"), str))
    addressing = network.bounded_items(networking["addressing"], 2)
    addresses = sorted(network.private_ip(item[key]) for item in addressing
                       for key in ("ipv4", "ipv6") if item.get(key))
    pod_addresses = sorted(network.private_ip(item["ip"])
                           for item in pod["status"].get("podIPs", [{"ip": pod["status"]["podIP"]}]))
    node_ip = network.private_ip(status["networking"]["node"])
    pod_node = network.private_ip(pod["status"]["hostIP"])
    agent_node = network.private_ip(agent["status"]["hostIP"])
    checkpoint(witness, "endpoint_pins", addressesMatch=bool(addresses) and addresses == pod_addresses,
               nodeMatchesPod=node_ip == pod_node, nodeMatchesAgent=node_ip == agent_node)
    if witness is not None:
        witness["observedEndpointAddresses"] = {
            "endpointNodeAddress": node_ip, "podNodeAddress": pod_node, "agentNodeAddress": agent_node,
            "endpointAddresses": addresses, "podAddresses": pod_addresses}
    require(addresses and addresses == pod_addresses
            and node_ip == network.private_ip(pod["status"]["hostIP"])
            == network.private_ip(agent["status"]["hostIP"]), UNAVAILABLE)
    return {"uid": metadata["uid"], "podUid": identity(pod)[0], "endpointId": endpoint_id,
            "securityIdentity": security_id, "nodeAddress": node_ip, "addresses": addresses}


def endpoint_revision(fields, binding, pod, witness=None):
    checkpoint(witness, "endpoint_projection_fields", fieldCount=len(fields),
               numericFieldsValid=len(fields) >= 4 and all(
                   isinstance(value, str) and re.fullmatch(r"[0-9]{1,19}", value) is not None
                   for value in fields[:4]))
    require(len(fields) == 7 and all(re.fullmatch(r"[0-9]{1,19}", value) for value in fields[:4]),
            UNAVAILABLE)
    endpoint_id, security_id, desired, realized = map(int, fields[:4])
    checkpoint(witness, "endpoint_projection_pins", endpointIdMatches=endpoint_id == binding["endpointId"],
               securityIdMatches=security_id == binding["securityIdentity"],
               policyModeRecognized=fields[4] in ("none", "ingress", "egress", "both"),
               namespaceFieldMatches=fields[5] == pod["metadata"]["namespace"],
               podFieldMatches=fields[6] == pod["metadata"]["name"])
    require(endpoint_id == binding["endpointId"] and security_id == binding["securityIdentity"]
            and fields[4] in ("none", "ingress", "egress", "both")
            and fields[5] == pod["metadata"]["namespace"] and fields[6] == pod["metadata"]["name"], UNAVAILABLE)
    checkpoint(witness, "endpoint_revision_bounds", desiredRevisionValid=0 <= desired < 2**63,
               realizedRevisionValid=0 <= realized < 2**63, realizedAheadOfDesired=realized > desired)
    # Cilium v1.18.5 UpdatePolicy advances policyRevision for a no-op without
    # updating nextPolicyRevision; its API exposes both independent counters.
    # https://github.com/cilium/cilium/blob/v1.18.5/pkg/endpoint/policy.go#L672-L718
    # https://github.com/cilium/cilium/blob/v1.18.5/pkg/endpoint/api.go#L452-L486
    require(0 <= realized < 2**63 and 0 <= desired < 2**63, UNAVAILABLE)
    return {"endpointId": endpoint_id, "securityIdentity": security_id,
            "desiredPolicyRevision": desired, "realizedPolicyRevision": realized,
            "policyEnabled": fields[4]}


def snapshot(setup, target, temporary_name=None, witness=None):
    if witness is not None:
        witness.update(complete=False, networkValidated=False)
    try:
        return _snapshot(setup, target, temporary_name, witness)
    except READ_ERRORS + (AttributeError, IndexError) as error:
        if witness is not None:
            witness["failureKind"] = (
                "command-timeout" if isinstance(error, subprocess.TimeoutExpired) else
                "transport-timeout" if isinstance(error, TimeoutError) else
                "constraint" if isinstance(error, (Failure, AssertionError)) else
                "shape" if isinstance(error, (KeyError, TypeError, AttributeError, IndexError)) else
                "format" if isinstance(error, ValueError) else
                "transport" if isinstance(error, (OSError, subprocess.SubprocessError)) else "operation")
        if isinstance(error, (AttributeError, IndexError)):
            raise Failure(UNAVAILABLE) from None
        raise


def _snapshot(setup, target, temporary_name, witness):
    # Never omit a KNP with the same name as the temporary Cilium policy.
    checkpoint(witness, "network_snapshot")
    base = network.snapshot(setup, target)
    checkpoint(witness, "network_validated")
    if witness is not None:
        witness["networkValidated"] = True
        witness["validatedNetworkFacts"] = copy.deepcopy(base["facts"])
    checkpoint(witness, "origin_recheck")
    network.selected_origin(setup)
    anchors = {}
    anchor_kinds = {}

    def read(path, kind):
        value = api_read(setup, path, witness, f"{kind}_read")
        checkpoint(witness, f"{kind}_identity", **identity_facts(value))
        network.metadata(value)
        anchors[path] = value
        anchor_kinds[path] = kind
        return value

    namespace = read("/api/v1/namespaces/kube-system", "namespace")
    config = read(CONFIG, "configmap")
    daemonset = read(DAEMONSET, "daemonset")
    account = read(core("kube-system", "serviceaccounts", "cilium"), "account")
    checkpoint(witness, "resource_names", namespaceMatches=namespace["metadata"]["name"] == "kube-system",
               configmapMatches=config["metadata"]["name"] == "cilium-config"
               and config["metadata"].get("namespace") == "kube-system",
               daemonsetMatches=daemonset["metadata"]["name"] == "cilium"
               and daemonset["metadata"].get("namespace") == "kube-system",
               accountMatches=account["metadata"]["name"] == "cilium"
               and account["metadata"].get("namespace") == "kube-system")
    require(namespace["metadata"]["name"] == "kube-system"
            and config["metadata"]["name"] == "cilium-config"
            and config["metadata"].get("namespace") == "kube-system"
            and daemonset["metadata"]["name"] == "cilium"
            and daemonset["metadata"].get("namespace") == "kube-system"
            and account["metadata"].get("namespace") == "kube-system"
            and account["metadata"].get("name") == "cilium", UNAVAILABLE)
    listed = api_read(setup, core("kube-system", "pods"), witness, "agent_list_read")
    checkpoint(witness, "agent_select", objectShape=shape(listed.get("items")),
               count=len(listed["items"]) if isinstance(listed.get("items"), list) else None)
    candidates = [pod for pod in network.bounded_items(listed["items"], 64)
                  if pod.get("spec", {}).get("nodeName") == "bridge-native-worker"
                  and pod["metadata"].get("labels", {}).get("k8s-app") == "cilium"]
    checkpoint(witness, "agent_select", count=len(candidates))
    require(len(candidates) == 1, UNAVAILABLE)
    listed_agent = candidates[0]
    agent_path = core("kube-system", "pods", listed_agent["metadata"]["name"])
    agent = read(agent_path, "agent")
    reference = owner(agent, "DaemonSet")
    checkpoint(witness, "agent_identity", listIdentityMatches=identity(agent) == identity(listed_agent),
               uidMatches=identity(agent)[0] == identity(listed_agent)[0],
               resourceVersionMatches=identity(agent)[1] == identity(listed_agent)[1],
               namespaceMatches=agent["metadata"].get("namespace") == "kube-system",
               ownerMatches=reference is not None and reference["name"] == "cilium"
               and reference["uid"] == identity(daemonset)[0])
    require(identity(agent) == identity(listed_agent) and agent["metadata"].get("namespace") == "kube-system"
            and reference is not None and reference["name"] == "cilium"
            and reference["uid"] == identity(daemonset)[0], UNAVAILABLE)
    checkpoint(witness, "agent_layout", agentSpecShape=shape(agent.get("spec")),
               statusShape=shape(agent.get("status")), templateShape=shape(daemonset.get("spec", {}).get("template")))
    checkpoint(witness, "agent_layout", containersShape=shape(agent["spec"].get("containers")),
               statusesShape=shape(agent["status"].get("containerStatuses")),
               templateShape=shape(daemonset["spec"]["template"].get("spec")))
    containers = [item for item in agent["spec"]["containers"] if item["name"] == "cilium-agent"]
    statuses = [item for item in agent["status"]["containerStatuses"] if item["name"] == "cilium-agent"]
    template = daemonset["spec"]["template"]
    expected_containers = [item for item in template["spec"]["containers"] if item["name"] == "cilium-agent"]
    checkpoint(witness, "agent_constraints", containerCount=len(containers), statusCount=len(statuses),
               templateContainerCount=len(expected_containers))
    require(len(containers) == len(statuses) == len(expected_containers) == 1, UNAVAILABLE)
    checkpoint(witness, "agent_constraints", ready=statuses[0].get("ready") is True,
               containerIdPresent=bool(statuses[0].get("containerID")),
               restartCountValid=type(statuses[0].get("restartCount")) is int,
               serviceAccountMatches=agent["spec"].get("serviceAccountName")
               == template["spec"].get("serviceAccountName") == "cilium",
               imageMatchesTemplate=expected_containers[0].get("image") == containers[0].get("image"),
               imageHasDigest=isinstance(containers[0].get("image"), str) and "@sha256:" in containers[0]["image"],
               imageExpectedVersion=isinstance(containers[0].get("image"), str) and re.fullmatch(
                   r"quay\.io/cilium/cilium:v1\.18\.5(?:@sha256:[a-f0-9]{64})?", containers[0]["image"]) is not None)
    require(len(containers) == len(statuses) == 1 and statuses[0].get("ready") is True
            and statuses[0].get("containerID") and type(statuses[0].get("restartCount")) is int
            and agent["spec"].get("serviceAccountName") == template["spec"].get("serviceAccountName") == "cilium"
            and len(expected_containers) == 1 and expected_containers[0]["image"] == containers[0]["image"]
            and re.fullmatch(r"quay\.io/cilium/cilium:v1\.18\.5(?:@sha256:[a-f0-9]{64})?", containers[0]["image"]),
            UNAVAILABLE)
    checkpoint(witness, "agent_selector", selectorShape=shape(daemonset["spec"].get("selector")))
    selector_matches = network.matches(daemonset["spec"]["selector"], agent["metadata"].get("labels", {}))
    checkpoint(witness, "agent_selector", selectorMatches=selector_matches)
    require(selector_matches, UNAVAILABLE)
    checkpoint(witness, "configmap_fields", dataShape=shape(config.get("data")))
    desired_config = configmap_facts(config)
    if witness is not None:
        witness["observedConfigMap"] = desired_config
    checkpoint(witness, "config_exec", exitStatus=None, timedOut=False)
    fields = read_projection(agent, witness=witness)
    effective_config = effective_configuration(fields, witness)
    if witness is not None:
        witness["observedEffectiveConfig"] = dict(effective_config)
    checkpoint(witness, "kube_proxy_exec", exitStatus=None, timedOut=False)
    status_fields = read_kube_proxy_projection(agent, witness=witness)
    effective_config["kubeProxyReplacement"] = kube_proxy_mode(status_fields, witness)
    if witness is not None:
        witness["observedEffectiveConfig"] = dict(effective_config)
    expected = (desired_config["policyCIDRMatchMode"] == []
                and desired_config["policyEnforcementMode"] == "default"
                and desired_config["kubeProxyReplacement"] == "false"
                and effective_config == {"policyCIDRMatchMode": [], "ciliumNetworkPolicyEnabled": True,
                                         "kubernetesNetworkPolicyEnabled": True, "kubeProxyReplacement": False})
    checkpoint(witness, "config_defaults", configurationMatches=expected)
    bindings, endpoints = {}, []
    for item in base["actor"]["pods"]:
        pod = base["actor"]["anchors"][core(base["actor"]["namespace"], "pods", item["name"])]
        path = resource(base["actor"]["namespace"], "ciliumendpoints", item["name"], CILIUM)
        endpoint = api_read(setup, path, witness, "endpoint_read")
        binding = endpoint_binding(endpoint, pod, agent, witness)
        checkpoint(witness, "endpoint_exec", exitStatus=None, timedOut=False)
        fields = read_projection(agent, binding["endpointId"], witness=witness)
        revision = endpoint_revision(fields, binding, pod, witness)
        expected &= revision["policyEnabled"] in ("egress", "both")
        current = api_read(setup, path, witness, "endpoint_recheck")
        current_binding = endpoint_binding(current, pod, agent, witness)
        checkpoint(witness, "endpoint_snapshot_match", bindingMatches=current_binding == binding)
        require(current_binding == binding, UNAVAILABLE)
        bindings[path] = binding
        endpoints.append({"identity": network.metadata(current), "podUid": item["uid"], **revision})
    baseline = policies(setup, base["actor"]["namespace"], temporary_name, witness)
    policy_identities = {}
    policy_facts = []
    for index, policy in enumerate(baseline):
        checkpoint(witness, "cnp_digest", index=index, specShape=shape(policy.get("spec")),
                   specsShape=shape(policy.get("specs")), managedFieldsShape=shape(policy["metadata"].get("managedFields", [])))
        path = resource(base["actor"]["namespace"], "ciliumnetworkpolicies", policy["metadata"]["name"], CILIUM)
        policy_identities[path] = policy_authority(policy)
        policy_facts.append({"identity": network.metadata(policy),
                             "specDigest": network.spec_digest({"spec": policy.get("spec"), "specs": policy.get("specs")})})
    for path, previous in anchors.items():
        current = api_read(setup, path, witness, "anchor_read")
        checkpoint(witness, "anchor_identity", anchorKind=anchor_kinds[path], **identity_facts(current))
        matched = identity(current) == identity(previous)
        checkpoint(witness, "anchor_identity", anchorKind=anchor_kinds[path], listIdentityMatches=matched,
                   uidMatches=identity(current)[0] == identity(previous)[0],
                   resourceVersionMatches=identity(current)[1] == identity(previous)[1])
        require(matched, UNAVAILABLE)
    current_policies = policies(setup, base["actor"]["namespace"], temporary_name, witness)
    checkpoint(witness, "cnp_recheck", count=len(current_policies))
    matched = {resource(base["actor"]["namespace"], "ciliumnetworkpolicies", value["metadata"]["name"], CILIUM):
               policy_authority(value) for value in current_policies} == policy_identities
    checkpoint(witness, "cnp_recheck", count=len(current_policies), authorityMatches=matched)
    require(matched, UNAVAILABLE)
    base["facts"]["cilium"] = {
        "configMap": desired_config, "effectiveAgentConfig": effective_config,
        "configurationMatchesExpected": expected, "agent": network.metadata(agent),
        "daemonSet": network.metadata(daemonset), "endpoints": endpoints,
        "baselineCiliumNetworkPolicies": policy_facts, "policyRevisionIsNotRuleSpecificProof": True}
    base["stable"]["cilium"] = {
        "anchors": {path: identity(value) for path, value in anchors.items()},
        "configDigest": network.spec_digest(config.get("data")), "effectiveConfig": effective_config,
        "agentProcess": (statuses[0]["containerID"], statuses[0]["restartCount"]),
        "endpoints": bindings, "policies": policy_identities}
    checkpoint(witness, "network_recheck")
    current_base = network.snapshot(setup, target)
    require(current_base["stable"] == {
        key: value for key, value in base["stable"].items() if key != "cilium"}, UNAVAILABLE)
    base["ready"] = current_base["ready"]
    base["actor"] = current_base["actor"]
    if witness is not None:
        witness["complete"] = True
    checkpoint(witness, "complete")
    return base
