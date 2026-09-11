"""Failure-only exact API egress experiment; never changes acceptance results."""

import copy
from datetime import datetime, timezone
import ipaddress
import hashlib
import json
import os
import re
import secrets
import time
import urllib.parse

from api_gate import CORE_REVISION
from api_outcome_diagnostics import collect as api_outcomes
from native_api import CORE, Failure, command, core, require, resource
from observation_diagnostics import (
    READ_ERRORS, identity, recheck_actor, resolve_actor,
)

NETWORK = "/apis/networking.k8s.io/v1"
CILIUM = "/apis/cilium.io/v2"
FAILURE = "Deadline: core-issued current observer capability"
UNAVAILABLE = "Observer network diagnostic provenance unavailable"
API_SERVICE = core("default", "services", "kubernetes")
API_ENDPOINTS = core("default", "endpoints", "kubernetes")
SELECTOR_KEYS = ("kars.azure.com/sandbox", "pod-template-hash")


def loopback_origin(server):
    require(isinstance(server, str) and len(server) <= 256
            and not any(character.isspace() or ord(character) < 32 for character in server), UNAVAILABLE)
    endpoint = urllib.parse.urlsplit(server)
    require(endpoint.scheme == "https" and endpoint.hostname and "%" not in endpoint.hostname
            and endpoint.username is None and endpoint.password is None
            and endpoint.path in ("", "/") and "?" not in server and "#" not in server, UNAVAILABLE)
    address = ipaddress.ip_address(endpoint.hostname)
    port = endpoint.port if endpoint.port is not None else 443
    require(address.is_loopback and 0 < port <= 65535, UNAVAILABLE)
    return str(address), port


def client_origin(setup):
    origin = loopback_origin(setup.admin.server)
    require(origin == loopback_origin(setup.cluster["server"])
            and type(setup.admin.port) is int
            and origin == (str(ipaddress.ip_address(setup.admin.host)), setup.admin.port), UNAVAILABLE)
    return origin


def selected_origin(setup):
    config = json.loads(command("kubectl", "config", "view", "--minify", "-o", "json", timeout=10))
    require(isinstance(config, dict), UNAVAILABLE)
    expected = "kind-bridge-native"
    contexts = bounded_items(config["contexts"], 1)
    clusters = bounded_items(config["clusters"], 1)
    users = bounded_items(config["users"], 1)
    require(len(contexts) == len(clusters) == len(users) == 1
            and all(isinstance(value, dict) for value in contexts + clusters + users), UNAVAILABLE)
    binding, cluster = contexts[0].get("context"), clusters[0].get("cluster")
    require(isinstance(binding, dict) and isinstance(cluster, dict), UNAVAILABLE)
    require(config.get("current-context") == expected
            and contexts[0]["name"] == clusters[0]["name"] == users[0]["name"] == expected
            and binding.get("cluster") == expected and binding.get("user") == expected, UNAVAILABLE)
    require(not cluster.get("proxy-url") and not cluster.get("insecure-skip-tls-verify")
            and not cluster.get("tls-server-name"), UNAVAILABLE)
    origin = loopback_origin(cluster["server"])
    require(origin == client_origin(setup), UNAVAILABLE)
    return origin


def sandbox_authority(value):
    """Keep JSON types distinct as well as every non-status authority field."""
    identity(value)
    authority = copy.deepcopy({key: item for key, item in value.items() if key != "status"})
    authority["metadata"].pop("resourceVersion", None)
    fields = bounded_items(authority["metadata"].get("managedFields", []), 64)
    require(all(isinstance(field, dict) for field in fields), UNAVAILABLE)
    # Status-subresource field ownership is bookkeeping, not a new authority.
    authority["metadata"]["managedFields"] = [
        field for field in fields if field.get("subresource") != "status"]
    observed = value["status"]["serviceObservation"]
    require(observed.get("phase") in ("Prepared", "Ready"), UNAVAILABLE)
    authority["serviceObservation"] = {
        key: item for key, item in observed.items() if key not in ("phase", "reason")}
    return json.dumps(authority, sort_keys=True, separators=(",", ":"))


def recheck_snapshot_actor(setup, actor, source_path):
    before = actor["anchors"][source_path]
    recheck_actor(setup, {**actor, "anchors": {
        path: value for path, value in actor["anchors"].items() if path != source_path}})
    current = setup.admin.get(source_path)
    require(sandbox_authority(current) == sandbox_authority(before), UNAVAILABLE)
    actor["anchors"][source_path] = current
    return current


def bounded_items(value, maximum):
    require(isinstance(value, list) and len(value) <= maximum, UNAVAILABLE)
    return value


def metadata(value):
    identity(value)
    result = {key: value["metadata"][key] for key in ("name", "uid", "resourceVersion")}
    if value["metadata"].get("namespace"):
        result["namespace"] = value["metadata"]["namespace"]
    require(all(isinstance(item, str) and re.fullmatch(r"[A-Za-z0-9_.:-]{1,253}", item)
                for item in result.values()), UNAVAILABLE)
    return result


def spec_digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def matches(selector, labels):
    require(isinstance(selector, dict) and set(selector) <= {"matchLabels", "matchExpressions"}
            and isinstance(labels, dict), UNAVAILABLE)
    exact = selector.get("matchLabels", {})
    require(isinstance(exact, dict) and len(exact) <= 32
            and all(isinstance(key, str) and isinstance(value, str) for key, value in exact.items()),
            UNAVAILABLE)
    result = all(labels.get(key) == value for key, value in exact.items())
    for expression in bounded_items(selector.get("matchExpressions", []), 16):
        require(isinstance(expression, dict) and set(expression) <= {"key", "operator", "values"}
                and isinstance(expression.get("key"), str), UNAVAILABLE)
        key, operator = expression["key"], expression.get("operator")
        values = bounded_items(expression.get("values", []), 16)
        require(all(isinstance(value, str) for value in values), UNAVAILABLE)
        if operator in ("In", "NotIn"):
            require(bool(values), UNAVAILABLE)
            result &= labels.get(key) in values if operator == "In" else labels.get(key) not in values
        elif operator in ("Exists", "DoesNotExist"):
            require(not values, UNAVAILABLE)
            result &= key in labels if operator == "Exists" else key not in labels
        else:
            raise Failure(UNAVAILABLE)
    return result


def private_ip(value):
    require(isinstance(value, str) and len(value) <= 45, UNAVAILABLE)
    address = ipaddress.ip_address(value)
    private_ranges = ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fc00::/7")
    require(any(address in ipaddress.ip_network(cidr) for cidr in private_ranges) and not any((
        address.is_loopback, address.is_link_local, address.is_multicast,
        address.is_unspecified, address.is_reserved, getattr(address, "scope_id", None),
    )), UNAVAILABLE)
    return str(address)


def api_targets(service, endpoints):
    for value in (service, endpoints):
        require(value["metadata"].get("namespace") == "default"
                and value["metadata"].get("name") == "kubernetes", UNAVAILABLE)
        metadata(value)
    spec = service["spec"]
    require(spec.get("type", "ClusterIP") == "ClusterIP" and not spec.get("externalIPs")
            and not spec.get("externalName"), UNAVAILABLE)
    ports = [port for port in bounded_items(spec["ports"], 8)
             if port.get("name") == "https" and port.get("protocol", "TCP") == "TCP"]
    require(len(ports) == 1 and type(ports[0].get("port")) is int and ports[0]["port"] == 443,
            UNAVAILABLE)
    require(ports[0].get("targetPort") in (6443, "https"), UNAVAILABLE)
    addresses = bounded_items(spec.get("clusterIPs", [spec["clusterIP"]]), 2)
    require(addresses and spec["clusterIP"] in addresses, UNAVAILABLE)
    targets = {(private_ip(address), 443) for address in addresses}
    endpoint_targets = set()
    for subset in bounded_items(endpoints.get("subsets", []), 8):
        ready = bounded_items(subset.get("addresses", []), 8)
        ports = [port for port in bounded_items(subset.get("ports", []), 8)
                 if port.get("name") == "https" and port.get("protocol", "TCP") == "TCP"]
        require(ready and len(ports) == 1 and type(ports[0].get("port")) is int
                and ports[0]["port"] == 6443, UNAVAILABLE)
        endpoint_targets.update((private_ip(address["ip"]), ports[0]["port"]) for address in ready)
    require(endpoint_targets and len(targets | endpoint_targets) <= 16, UNAVAILABLE)
    return [{"address": address, "port": port}
            for address, port in sorted(targets | endpoint_targets)]


def rule_match(rule, target):
    """Declared Kubernetes rule matching only; CNI entity/DNAT behavior is unknown."""
    require(isinstance(rule, dict) and set(rule) <= {"to", "ports"}, UNAVAILABLE)
    ports = bounded_items(rule.get("ports", []), 16)
    port_match = not ports
    unknown_port = False
    for port in ports:
        require(isinstance(port, dict) and set(port) <= {"port", "endPort", "protocol"}, UNAVAILABLE)
        if port.get("protocol", "TCP") != "TCP":
            continue
        start, end = port.get("port"), port.get("endPort", port.get("port"))
        if start is None:
            port_match = True
        elif isinstance(start, str):
            unknown_port = True
        else:
            require(type(start) is int and type(end) is int and 0 < start <= end <= 65535, UNAVAILABLE)
            port_match |= start <= target["port"] <= end
    peers = bounded_items(rule.get("to", []), 16)
    address_match, unknown_peer = not peers, False
    address = ipaddress.ip_address(target["address"])
    for peer in peers:
        require(isinstance(peer, dict)
                and set(peer) <= {"ipBlock", "namespaceSelector", "podSelector"}, UNAVAILABLE)
        if not peer:
            address_match = True
        elif "ipBlock" in peer:
            require(set(peer) == {"ipBlock"}, UNAVAILABLE)
            block = peer["ipBlock"]
            network = ipaddress.ip_network(block["cidr"])
            exceptions = [ipaddress.ip_network(value)
                          for value in bounded_items(block.get("except", []), 16)]
            require(all(value.subnet_of(network) for value in exceptions), UNAVAILABLE)
            address_match |= address in network and not any(address in value for value in exceptions)
        else:
            unknown_peer = True
    if address_match and port_match:
        return True
    if (address_match or unknown_peer) and (port_match or unknown_port):
        return None
    return False


def policy_facts(policy, pods, targets):
    spec = policy["spec"]
    types = bounded_items(spec.get("policyTypes", []), 2)
    require(types and set(types) <= {"Ingress", "Egress"}, UNAVAILABLE)
    selected = [identity(pod)[0] for pod in pods
                if matches(spec["podSelector"], pod["metadata"].get("labels", {}))]
    rules = bounded_items(spec.get("egress", []), 32)
    return {"identity": metadata(policy), "specDigest": spec_digest(spec),
            "policyTypes": types, "selectedObserverPodUids": selected,
            "egressRuleCount": len(rules), "apiDeclaredMatches": [
                {**target, "ruleMatches": [rule_match(rule, target) for rule in rules]}
                for target in targets
            ] if "Egress" in types and selected else []}


def snapshot(setup, target, temporary_name=None):
    require(isinstance(target, dict) and target.get("workspace") == CORE
            and target.get("task") == "native-observation-task" and target.get("uid"), UNAVAILABLE)
    actor = resolve_actor(setup, "router", target)
    namespace = actor["namespace"]
    source_path = resource(CORE, "karssandboxes", target["sandbox"])
    source = actor["anchors"][source_path]
    ns = actor["anchors"][f"/api/v1/namespaces/{namespace}"]
    deployment = actor["anchors"][resource(namespace, "deployments", target["sandbox"], "/apis/apps/v1")]
    require(ns["metadata"].get("annotations", {}).get("kars.azure.com/sandbox-uid") == target["uid"]
            and deployment["metadata"].get("annotations", {}).get("kars.azure.com/credential-sandbox-uid")
            == target["uid"]
            and deployment["metadata"].get("annotations", {}).get("kars.azure.com/credential-namespace-uid")
            == identity(ns)[0], UNAVAILABLE)
    pods = [actor["anchors"][core(namespace, "pods", item["name"])] for item in actor["pods"]]
    selector = {"matchLabels": {key: pods[0]["metadata"].get("labels", {}).get(key)
                                 for key in SELECTOR_KEYS}}
    require(all(isinstance(value, str) and re.fullmatch(r"[a-z0-9][-a-z0-9]{0,62}", value)
                for value in selector["matchLabels"].values()), UNAVAILABLE)
    pod_processes = []
    for pod in pods:
        spec = pod["spec"]
        routers = [container for container in spec["containers"] if container.get("name") == "inference-router"]
        statuses = [item for item in pod.get("status", {}).get("containerStatuses", [])
                    if item.get("name") == "inference-router"]
        require(matches(selector, pod["metadata"].get("labels", {}))
                and not spec.get("hostNetwork") and not spec.get("hostPID")
                and not spec.get("shareProcessNamespace") and not spec.get("ephemeralContainers")
                and spec.get("nodeName") == "bridge-native-worker"
                and len(routers) == len(statuses) == 1, UNAVAILABLE)
        security = routers[0].get("securityContext", {})
        run_as = security.get("runAsUser", spec.get("securityContext", {}).get("runAsUser"))
        require(type(run_as) is int and run_as == 1001
                and not security.get("privileged") and security.get("allowPrivilegeEscalation") is False
                and statuses[0].get("ready") is True and statuses[0].get("containerID")
                and type(statuses[0].get("restartCount")) is int, UNAVAILABLE)
        pod_processes.append((identity(pod)[0], statuses[0]["containerID"], statuses[0]["restartCount"]))
    all_pods = bounded_items(setup.admin.get(core(namespace, "pods"))["items"], 64)
    consumers = [pod for pod in all_pods if matches(selector, pod["metadata"].get("labels", {}))]
    require(sorted(identity(pod)[0] for pod in consumers) == sorted(item["uid"] for item in actor["pods"]),
            "Diagnostic selector has unknown or foreign consumers")
    require(all(pod["metadata"].get("namespace") == namespace
                and identity(pod) == identity(actor["anchors"][core(namespace, "pods", pod["metadata"]["name"])])
                for pod in consumers), UNAVAILABLE)
    for path in ("/api/v1/namespaces/default", API_SERVICE, API_ENDPOINTS):
        actor["anchors"][path] = setup.admin.get(path)
        metadata(actor["anchors"][path])
    require(actor["anchors"]["/api/v1/namespaces/default"]["metadata"].get("name") == "default", UNAVAILABLE)
    targets = api_targets(actor["anchors"][API_SERVICE], actor["anchors"][API_ENDPOINTS])
    policies = bounded_items(setup.admin.get(resource(namespace, "networkpolicies", group=NETWORK))["items"], 32)
    policies = [value for value in policies if value["metadata"].get("name") != temporary_name]
    require(all(value["metadata"].get("namespace") == namespace for value in policies), UNAVAILABLE)
    for policy in policies:
        path = resource(namespace, "networkpolicies", policy["metadata"]["name"], NETWORK)
        actor["anchors"][path] = policy
    facts = {"namespace": metadata(ns), "deployment": metadata(deployment),
             "pods": [{"identity": metadata(pod), "selector": selector,
                       "container": "inference-router", "configuredRunAsUser": 1001} for pod in pods],
             "apiService": metadata(actor["anchors"][API_SERVICE]),
             "apiEndpoints": metadata(actor["anchors"][API_ENDPOINTS]), "destinations": targets,
             "networkPolicies": [policy_facts(policy, pods, targets) for policy in policies],
             "cniBehaviorProven": False}
    source = recheck_snapshot_actor(setup, actor, source_path)
    stable = {
        "sandboxAuthority": sandbox_authority(source),
        "uids": {path: identity(value)[0] for path, value in actor["anchors"].items()},
        "generation": source["metadata"].get("generation"),
        "deploymentGeneration": deployment["metadata"].get("generation"),
        "observerVersion": source["status"]["serviceObservation"]["version"],
        "selector": selector, "processes": sorted(pod_processes), "destinations": targets,
        "api": {path: identity(actor["anchors"][path]) for path in (API_SERVICE, API_ENDPOINTS)},
        "policies": {identity(value): value["spec"] for value in policies},
    }
    return {"actor": actor, "stable": stable, "facts": facts, "selector": selector,
            "namespace": ns, "deployment": deployment,
            "ready": source["status"]["serviceObservation"]["phase"] == "Ready"}


def policy_plan(before, name):
    ports = sorted({target["port"] for target in before["facts"]["destinations"]})
    require(ports == [443, 6443] and all(type(target["port"]) is int for target in before["facts"]["destinations"]),
            UNAVAILABLE)
    return {"apiVersion": "cilium.io/v2", "kind": "CiliumNetworkPolicy",
            "metadata": {"name": name, "namespace": before["actor"]["namespace"],
                         "ownerReferences": [{"apiVersion": "apps/v1", "kind": "Deployment",
                             "name": before["deployment"]["metadata"]["name"],
                             "uid": identity(before["deployment"])[0],
                             "controller": False, "blockOwnerDeletion": False}]},
            "spec": {"endpointSelector": copy.deepcopy(before["selector"]),
                     "egress": [{"toEntities": ["kube-apiserver"],
                                 "toPorts": [{"ports": [{"protocol": "TCP", "port": str(port)}
                                                        for port in ports]}]}]}}


def remove_policy(setup, before, path, created):
    require(client_origin(setup) == before["apiOrigin"], "Diagnostic API origin changed before cleanup")
    ns = setup.admin.get(f'/api/v1/namespaces/{before["actor"]["namespace"]}')
    require(identity(ns)[0] == identity(before["namespace"])[0], "Diagnostic namespace changed before cleanup")
    current = setup.admin.optional(path)
    deleted = current is not None
    if current is not None:
        require(identity(current)[0] == identity(created)[0]
                and current["metadata"].get("name") == created["metadata"]["name"]
                and current["metadata"].get("namespace") == before["actor"]["namespace"],
                "Diagnostic policy was replaced; cleanup refused")
        setup.admin.request("DELETE", path, {
            "apiVersion": "v1", "kind": "DeleteOptions",
            "preconditions": {"uid": identity(created)[0], "resourceVersion": identity(current)[1]},
        }, expected=(200, 202))
    for _ in range(5):
        current = setup.admin.optional(path)
        if current is None:
            require(identity(setup.admin.get(f'/api/v1/namespaces/{before["actor"]["namespace"]}'))[0]
                    == identity(before["namespace"])[0], "Diagnostic namespace changed during cleanup")
            return "uid-rv-deletion-verified" if deleted else "already-absent-verified"
        require(current["metadata"].get("uid") == identity(created)[0], "Diagnostic policy name was reused")
        time.sleep(1)
    raise Failure("Diagnostic policy cleanup was not verified")


def collect(setup, target, failed_case):
    from observer_cilium_diagnostics import snapshot as cilium_snapshot

    result = {"diagnosticOnly": True, "originalResult": "failed", "available": False,
              "category": "not-eligible", "policyCreated": False, "cleanup": "not-required",
              "cniAcceptanceQualified": False, "experiment": "cilium-kube-apiserver-entity"}
    if (not isinstance(failed_case, dict) or failed_case.get("result") != "failed"
            or failed_case.get("failure") != FAILURE):
        return result
    before = created = path = None
    try:
        result["stage"] = "disposable-host"
        require(os.environ.get("GITHUB_ACTIONS") == "true"
                and os.environ.get("GITHUB_REPOSITORY") == "Azure/kars"
                and os.environ.get("CORE_REVISION") == CORE_REVISION, UNAVAILABLE)
        origin = selected_origin(setup)
        require(command("docker", "inspect", "bridge-native-worker", "--format",
                            '{{index .Config.Labels "io.x-k8s.kind.cluster"}}', timeout=10).strip()
                == "bridge-native", UNAVAILABLE)
        result["stage"] = "baseline-snapshot"
        result["baselineSnapshot"] = {}
        before = cilium_snapshot(setup, target, witness=result["baselineSnapshot"])
        before["apiOrigin"] = origin
        result.update(available=True, before=before["facts"], category="baseline-retained")
        if not before["facts"]["cilium"]["configurationMatchesExpected"]:
            result["category"] = "unexpected-cilium-configuration-no-intervention"
            return result
        if before["ready"]:
            result.update(category="already-ready-without-intervention", samePodObserverReady=True)
            return result
        isolated = {pod_uid for policy in before["facts"]["networkPolicies"]
                    if "Egress" in policy["policyTypes"] for pod_uid in policy["selectedObserverPodUids"]}
        if not {pod["uid"] for pod in before["actor"]["pods"]} <= isolated:
            result["category"] = "no-observed-egress-isolation-no-intervention"
            return result
        name = f"native-observer-api-{secrets.token_hex(8)}"
        result["stage"] = "pre-create-recheck"
        path = resource(before["actor"]["namespace"], "ciliumnetworkpolicies", name, CILIUM)
        require(setup.admin.optional(path) is None, "Diagnostic policy name is already occupied")

        def recheck(phase, temporary_name=None):
            result["observationStage"] = f"{phase}-snapshot"
            result["observationSnapshot"] = {}
            result.pop("stableChecks", None)
            result.pop("ciliumStableChecks", None)
            observed = cilium_snapshot(setup, target, temporary_name,
                                       witness=result["observationSnapshot"])
            result["observationStage"] = f"{phase}-stability"
            result["stableChecks"] = {
                key: observed["stable"].get(key) == before["stable"].get(key)
                for key in ("sandboxAuthority", "uids", "generation", "deploymentGeneration",
                            "observerVersion", "selector", "processes", "destinations", "api", "policies", "cilium")
            }
            result["ciliumStableChecks"] = {
                key: observed["stable"].get("cilium", {}).get(key)
                == before["stable"].get("cilium", {}).get(key)
                for key in ("anchors", "configDigest", "effectiveConfig", "agentProcess", "endpoints", "policies")
            }
            require(observed["stable"] == before["stable"], UNAVAILABLE)
            return observed

        current = recheck("pre-create")
        if current["ready"]:
            result.update(category="already-ready-without-intervention", samePodObserverReady=True)
            return result
        require(selected_origin(setup) == origin, "Diagnostic API origin changed before policy creation")
        desired = policy_plan(before, name)
        result["stage"] = "temporary-policy-create"
        result["cleanup"] = "creation-unconfirmed"
        candidate = setup.admin.create(resource(before["actor"]["namespace"], "ciliumnetworkpolicies", group=CILIUM), desired)
        require(candidate["metadata"].get("name") == name
                and candidate["metadata"].get("namespace") == before["actor"]["namespace"], UNAVAILABLE)
        metadata(candidate)
        created = candidate
        result.update(policyCreated=True, policy=metadata(created), cleanup="pending")
        require(created["spec"] == desired["spec"], "Diagnostic policy was mutated")
        started = datetime.now(timezone.utc)
        deadline = time.monotonic() + 60
        result["category"] = "no-progress-observed"
        result["stage"] = "same-pod-observation"
        while time.monotonic() < deadline:
            current = recheck("before-api", name)
            result["observationStage"] = "temporary-policy-identity"
            policy = setup.admin.get(path)
            require(identity(policy)[0] == identity(created)[0] and policy["spec"] == desired["spec"], UNAVAILABLE)
            result["observationStage"] = "api-outcomes"
            outcomes = api_outcomes(setup, "observer_router", target, started)
            current = recheck("after-api", name)
            result["duringApiOutcomes"] = outcomes
            result["duringCiliumEndpoints"] = current["facts"]["cilium"]["endpoints"]
            result["apiResponseObserved"] = outcomes["available"]
            result["samePodObserverReady"] = current["ready"]
            if outcomes["available"] or current["ready"]:
                result["category"] = "same-pod-progress-with-temporary-policy"
                break
            time.sleep(5)
        recheck("final", name)
    except READ_ERRORS:
        result["category"] = "provenance-or-operation-unavailable"
        if result.get("stage") == "baseline-snapshot" and result.get("baselineSnapshot"):
            result["baselineStoppingStage"] = result["baselineSnapshot"].get("lastStage")
        if result.get("observationStage"):
            result["observationStoppingStage"] = result["observationStage"]
    finally:
        if created is not None:
            try:
                result["cleanup"] = remove_policy(setup, before, path, created)
            except READ_ERRORS:
                result["cleanup"] = "unverified-or-refused"
                result["category"] = "cleanup-not-verified"
    return result
