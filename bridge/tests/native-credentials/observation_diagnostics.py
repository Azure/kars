"""Failure-only projection of fixed core stages; raw logs never enter evidence."""

import json
import subprocess

from native_api import BRIDGE, CORE, WRITER, Failure, command, core, require, resource

STAGES = frozenset("""
consumer_absent consumer_address consumer_credential consumer_lineage consumer_namespace
consumer_pods consumer_rollout consumer_rollout_pending
observer_bearer observer_binding observer_body observer_configuration observer_expiry
observer_grant_current observer_grant_read observer_http observer_metadata_client
observer_namespace_current observer_namespace_read observer_origin observer_privacy_revision
observer_recipient_account observer_recipient_current observer_recipient_namespace
observer_registration_current observer_registration_read observer_route_authorization
observer_route_identity observer_route_scope observer_scope_binding observer_scope_current
observer_secret_denial_result observer_secret_denial_review observer_target_current
observer_target_read observer_tls_client observer_transport observer_verifier
observer_workspace_current observer_workspace_read
rpc_admission rpc_audience_denial rpc_authority rpc_authorization rpc_bearer rpc_binding
rpc_body rpc_capacity rpc_controller_privacy rpc_credential_current rpc_credential_read
rpc_endpoint_available rpc_final_endpoint rpc_final_snapshot rpc_grant_current rpc_grant_read
rpc_headers rpc_initial_endpoint rpc_initial_snapshot rpc_namespaces rpc_proof_current
rpc_recipients rpc_registration rpc_request rpc_request_json rpc_runtime_privacy
rpc_service_identity rpc_target_current rpc_target_read rpc_writer_authority
verifier_account_read verifier_address verifier_audience_denial verifier_audience_review
verifier_binding verifier_body verifier_descriptor_current verifier_descriptor_read
verifier_endpoint verifier_exchange verifier_http verifier_identity_current verifier_namespace_read
verifier_proof_binding verifier_proof_json verifier_request verifier_service_current
verifier_service_read verifier_tls_client verifier_transport
""".split())

TARGETS = {
    "controller": "kars_controller::observation_privacy",
    "router": "kars_inference_router::observation_privacy",
}

CLIENT_FIELDS = (
    "client_initialized", "request_built", "service_entered", "dispatch_observable", "after_auth_dispatch",
    "response_headers", "config_observed", "https", "tls_verification",
    "root_ca_present", "token_file_only", "proxy_configured",
    "endpoint_environment_matches", "runtime_namespace_matches",
)

# Absent on older cores; absence is unknown, not a negative transport result.
CLIENT_TRANSPORT_FIELDS = (
    "transport_debug_observable", "transport_trace_observable",
    "tcp_connect_started", "tcp_connected", "http_handshake_complete",
)


def project(raw, component):
    if component not in TARGETS:
        return []
    records = []
    for line in raw.splitlines()[-512:]:
        try:
            value = json.loads(line)
        except (ValueError, TypeError):
            continue
        if not isinstance(value, dict) or value.get("target") != TARGETS.get(component):
            continue
        fields = value.get("fields")
        if not isinstance(fields, dict):
            continue
        stage, status = fields.get("stage"), fields.get("http_status")
        if fields.get("message") == "Private observation target client pending":
            if (component != "router" or stage != "observer_target_client"
                    or type(status) is not int or not (status == 0 or 100 <= status <= 599)
                    or any(type(fields.get(key)) is not bool for key in CLIENT_FIELDS)):
                continue
            record = {"stage": stage, "http_status": status,
                      **{key: fields[key] for key in CLIENT_FIELDS}}
            if any(key in fields for key in CLIENT_TRANSPORT_FIELDS):
                if any(type(fields.get(key)) is not bool for key in CLIENT_TRANSPORT_FIELDS):
                    continue
                record.update({key: fields[key] for key in CLIENT_TRANSPORT_FIELDS})
            if not records or records[-1] != record:
                records.append(record)
            continue
        if fields.get("message") != "Private observation readiness pending":
            continue
        if (not isinstance(stage, str) or stage not in STAGES or type(status) is not int
                or not (status == 0 or 100 <= status <= 599)
                or type(fields.get("timeout")) is not bool or type(fields.get("connect")) is not bool):
            continue
        record = {key: fields[key] for key in ("stage", "http_status", "timeout", "connect")}
        if not records or records[-1] != record:
            records.append(record)
    return records[-24:]


UNAVAILABLE = "Observation diagnostic provenance unavailable"
VERSION = "kars.azure.com/services-observer-version"
READ_ERRORS = (Failure, AssertionError, KeyError, TypeError, ValueError, RuntimeError,
               OSError, subprocess.SubprocessError)


def identity(value):
    metadata = value["metadata"]
    require(not metadata.get("deletionTimestamp")
            and all(isinstance(metadata.get(key), str) and metadata[key]
                    for key in ("uid", "resourceVersion")), UNAVAILABLE)
    return metadata["uid"], metadata["resourceVersion"]


def owner(value, kind):
    owners = [item for item in value["metadata"].get("ownerReferences", [])
              if item.get("controller") is True]
    if len(owners) != 1:
        return None
    value = owners[0]
    return value if (value.get("apiVersion") == "apps/v1" and value.get("kind") == kind
                     and value.get("name") and value.get("uid")) else None


def resolve_actor(setup, component, target):
    anchors = {}

    def read(path):
        value = setup.admin.get(path)
        identity(value)
        anchors[path] = value
        return value

    namespace_uid = deployment_uid = version = None
    if component == "controller":
        namespace, name, account, container = CORE, "kars-controller", "kars-controller", "controller"
        labels = {"app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "controller"}
    elif component == "bff":
        namespace, name, account, container = BRIDGE, "kars-bridge-bff", WRITER, "bff"
        labels = {"app.kubernetes.io/name": "kars-bridge", "app.kubernetes.io/component": "bff"}
    else:
        require(component == "router" and isinstance(target, dict)
                and target.get("workspace") == CORE, UNAVAILABLE)
        name = target["sandbox"]
        sandbox = read(resource(CORE, "karssandboxes", name))
        require(sandbox["metadata"].get("namespace") == CORE
                and sandbox["metadata"].get("name") == name
                and (target.get("uid") is None or identity(sandbox)[0] == target["uid"]), UNAVAILABLE)
        observed = sandbox.get("status", {}).get("serviceObservation") or {}
        namespace_uid, deployment_uid = observed.get("namespaceUid"), observed.get("deploymentUid")
        version = observed.get("version")
        require(observed.get("phase") in ("Prepared", "Ready")
                and isinstance(namespace_uid, str) and namespace_uid
                and isinstance(deployment_uid, str) and deployment_uid
                and isinstance(version, str) and version, UNAVAILABLE)
        namespace, account, container = f"kars-{name}", "sandbox", "inference-router"
        labels = {"kars.azure.com/sandbox": name}
    ns = read(f"/api/v1/namespaces/{namespace}")
    require(ns["metadata"].get("name") == namespace
            and (namespace_uid is None or identity(ns)[0] == namespace_uid), UNAVAILABLE)
    deployment = read(resource(namespace, "deployments", name, "/apis/apps/v1"))
    require(deployment["metadata"].get("namespace") == namespace
            and deployment["metadata"].get("name") == name
            and (deployment_uid is None or identity(deployment)[0] == deployment_uid), UNAVAILABLE)
    template = deployment.get("spec", {}).get("template", {})
    require(template.get("spec", {}).get("serviceAccountName") == account
            and all(template.get("metadata", {}).get("labels", {}).get(key) == value
                    for key, value in labels.items())
            and (version is None or template.get("metadata", {}).get("annotations", {}).get(VERSION)
                 == version), UNAVAILABLE)
    service_account = read(core(namespace, "serviceaccounts", account))
    require(service_account["metadata"].get("namespace") == namespace
            and service_account["metadata"].get("name") == account, UNAVAILABLE)
    pods = setup.admin.get(core(namespace, "pods"))["items"]
    candidates = [pod for pod in pods
                  if pod["metadata"].get("namespace") == namespace
                  and pod.get("spec", {}).get("serviceAccountName") == account
                  and (version is None or pod["metadata"].get("annotations", {}).get(VERSION) == version)
                  and all(pod["metadata"].get("labels", {}).get(key) == value
                          for key, value in labels.items())]
    require(len(candidates) <= 8, UNAVAILABLE)
    sources = []
    for pod in candidates:
        reference = owner(pod, "ReplicaSet")
        if reference is None:
            continue
        set_path = resource(namespace, "replicasets", reference["name"], "/apis/apps/v1")
        replica_set = setup.admin.get(set_path)
        lineage = owner(replica_set, "Deployment")
        if (identity(replica_set)[0] != reference["uid"]
                or replica_set["metadata"].get("namespace") != namespace
                or replica_set["metadata"].get("name") != reference["name"]
                or lineage is None or lineage["name"] != name
                or lineage["uid"] != identity(deployment)[0]):
            continue
        anchors[set_path] = replica_set
        pod_path = core(namespace, "pods", pod["metadata"]["name"])
        current = read(pod_path)
        require(identity(current) == identity(pod)
                and current["metadata"].get("namespace") == namespace
                and current["metadata"].get("name") == pod["metadata"]["name"], UNAVAILABLE)
        sources.append({"name": pod["metadata"]["name"], "uid": identity(pod)[0]})
    require(bool(sources), UNAVAILABLE)
    return {"anchors": anchors, "pods": sources, "namespace": namespace, "container": container,
            "username": f"system:serviceaccount:{namespace}:{account}",
            "serviceAccountUid": identity(service_account)[0]}


def recheck_actor(setup, actor):
    for path, before in actor["anchors"].items():
        require(identity(setup.admin.get(path)) == identity(before), UNAVAILABLE)


def sample(setup, component, target):
    result = {"component": component, "available": False, "records": []}
    try:
        actor = resolve_actor(setup, component, target)
        records = []
        for pod in actor["pods"]:
            raw = command("kubectl", "logs", "-n", actor["namespace"], pod["name"], "-c", actor["container"],
                          "--tail=512", "--limit-bytes=131072", "--request-timeout=10s", timeout=15)
            records.extend(project(raw, component))
        recheck_actor(setup, actor)
        result.update(available=bool(records), records=records[-24:])
    except READ_ERRORS:
        return result
    return result


def collect(setup, target):
    samples = [sample(setup, component, target) for component in ("controller", "router")]
    return {"available": all(item["available"] for item in samples), "samples": samples}
