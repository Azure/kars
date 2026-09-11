"""Actual CEL evaluation with explicit setup-admin authority, not BFF issuance."""

import base64
from datetime import datetime, timezone
import secrets

from native_api import CORE, Failure, Setup, core, require, resource, uid

NETWORK = "/apis/networking.k8s.io/v1"
PENDING = "kars.azure.com/credential-rebind-pending"


class Cases:
    def __init__(self):
        self.setup = Setup()
        self.api = self.setup.admin
        self.created = []
        self.results = {"actor": "setup-admin", "bearerIssuanceQualified": False}

    def create(self, path, body):
        value = self.api.create(path, body)
        self.created.append(f"{path}/{value['metadata']['name']}")
        return value

    def namespace(self, name, isolated=False):
        metadata = {"name": name}
        if isolated:
            metadata["labels"] = {"kars.azure.com/isolated": "strict"}
        return self.create("/api/v1/namespaces", {
            "apiVersion": "v1", "kind": "Namespace", "metadata": metadata,
        })

    def denied(self, method, path, body, policy, message, code):
        status, result = self.api.request(method, path, body, expected=(code,),
                                         patch_type="application/merge-patch+json" if method == "PATCH" else None)
        require(policy in result.get("message", "") and message in result["message"],
                "Native denial did not identify the intended admission predicate")
        return status

    def stores(self):
        namespace = "native-admission-stores"
        workspace = self.namespace(namespace)
        name = "kars-provider-native"
        secret = self.create(core(namespace, "secrets"), {
            "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
            "metadata": {"name": name, "namespace": namespace},
        })
        grant = self.create(resource(namespace, "karscredentialgrants"), {
            "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsCredentialGrant",
            "metadata": {"name": "workspace", "namespace": namespace},
            "spec": {"workspaceUid": uid(workspace), "writers": [], "enabled": True,
                     "integrationStores": [{"purpose": "provider-default",
                                            "secret": {"name": name, "uid": uid(secret)}}]},
        })
        path = core(namespace, "secrets", name)
        self.api.patch(path, {"metadata": {"annotations": {
            "kars.azure.com/credential-store-grant-uid": uid(grant),
        }}})
        cases = []
        for field in ["data", "stringData"]:
            value = secrets.token_hex(24)
            encoded = base64.b64encode(value.encode()).decode()
            updated = self.api.patch(path, {field: {"API_KEY": encoded if field == "data" else value}})
            require(uid(updated) == uid(secret) and updated["data"]["API_KEY"] == encoded,
                    "Valid enrolled Secret keys did not persist through native admission")
            cases.append({"field": field, "keys": "allowed", "status": 200})
        for fields in [{"data": ["PASSWORD"]}, {"stringData": ["PASSWORD"]},
                       {"data": ["API_KEY"], "stringData": ["PASSWORD"]},
                       {"data": ["PASSWORD"], "stringData": ["API_KEY"]}]:
            before = self.api.get(path)
            body = {"metadata": {"uid": uid(before),
                                 "resourceVersion": before["metadata"]["resourceVersion"]}}
            for field, keys in fields.items():
                value = secrets.token_hex(24)
                body[field] = {key: base64.b64encode(value.encode()).decode() if field == "data" else value
                               for key in keys}
            code = self.denied("PATCH", path, body, "kars-credential-enrolled-store-shape",
                               "exact UID and purpose", 422)
            after = self.api.get(path)
            require(uid(after) == uid(before) and after.get("data") == before.get("data")
                    and after["metadata"]["resourceVersion"] == before["metadata"]["resourceVersion"],
                    "Forbidden enrolled keys mutated the native Secret")
            cases.append({"fields": sorted(fields), "keys": "forbidden", "status": code})
        before = self.api.get(path)
        self.api.patch(resource(namespace, "karscredentialgrants", "workspace"), {"spec": {
            "integrationStores": [{"purpose": "provider-default",
                                    "secret": {"name": name, "uid": "different-secret-uid"}}],
        }})
        code = self.denied("PATCH", path, {
            "metadata": {"uid": uid(before), "resourceVersion": before["metadata"]["resourceVersion"]},
            "stringData": {"API_KEY": secrets.token_hex(24)},
        }, "kars-credential-enrolled-store-shape", "exact UID and purpose", 422)
        require(self.api.get(path)["data"] == before["data"], "Wrong enrolled UID changed secret values")
        cases.append({"identity": "wrong-secret-uid", "status": code})
        self.results["enrolledStoreKeys"] = cases

    def rebind(self):
        namespace = "native-admission-rebind"
        self.namespace(namespace)
        controller = self.setup.actor(CORE, "kars-controller")
        cases = []
        for variant in ["absent", "null", "non-null"]:
            name = f"resume-{variant}"
            task = self.create(resource(namespace, "karstasks"), {
                "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTask",
                "metadata": {"name": name, "namespace": namespace, "annotations": {PENDING: "true"}},
                "spec": {"objective": "Native rebind admission evaluation",
                         "envelope": {"tier": 1, "authorityCeiling": 1, "delegationDepth": 0},
                         "execution": {"launch": True}},
            })
            path = resource(namespace, "karstasks", name)
            status = {"executionPhase": "CredentialsPaused",
                      "observedGeneration": task["metadata"]["generation"],
                      "conditions": [{"type": "Ready", "status": "False",
                                      "lastTransitionTime": datetime.now(timezone.utc).isoformat(),
                                      "reason": "NativeAdmissionProof", "message": "Synthetic paused fixture"}]}
            if variant != "absent":
                status["envelopeDigest"] = None if variant == "null" else "sha256:" + "a" * 64
            controller.request("PATCH", path + "/status", [
                {"op": "test", "path": "/metadata/uid", "value": uid(task)},
                {"op": "test", "path": "/metadata/resourceVersion", "value": task["metadata"]["resourceVersion"]},
                {"op": "add", "path": "/status", "value": status},
            ], patch_type="application/json-patch+json")
            current = self.api.get(path)
            observed = current["status"]
            wire_kind = ("absent" if "envelopeDigest" not in observed else
                         "null" if observed["envelopeDigest"] is None else "non-null")
            require((variant == "non-null") == (wire_kind == "non-null"),
                    "Native digest status fixture was not retained")
            body = {"metadata": {"uid": uid(current),
                                 "resourceVersion": current["metadata"]["resourceVersion"],
                                 "annotations": {PENDING: None}}}
            if variant == "non-null":
                code = self.denied("PATCH", path, body, "kars-credential-rebind-authority",
                                   "current paused authority", 422)
                require(self.api.get(path)["metadata"]["annotations"].get(PENDING) == "true",
                        "Non-null digest denial removed the credential hold")
            else:
                code, resumed = self.api.request("PATCH", path, body, patch_type="application/merge-patch+json")
                require(uid(resumed) == uid(task) and resumed["spec"]["execution"]["launch"] is True
                        and PENDING not in resumed["metadata"].get("annotations", {}),
                        "Current absent/null-digest pause could not resume nondestructively")
            # Kubernetes may normalize an explicitly submitted null to absence
            # under a non-nullable CRD field. Record the actual persisted wire
            # shape rather than claiming a branch that never reached CEL.
            cases.append({"submittedDigest": variant, "storedDigest": wire_kind, "status": code})
        self.results["pausedDigestResume"] = cases
        self.results["pausedStatusWriter"] = f"system:serviceaccount:{CORE}:kars-controller"

    def exposure(self):
        namespace = "native-admission-isolated"
        self.namespace(namespace, isolated=True)
        cases = []
        def service(name, kind=None):
            spec = {"selector": {"app": "native-fixture"}, "ports": [{"port": 8443}]}
            if kind:
                spec["type"] = kind
            return {"apiVersion": "v1", "kind": "Service",
                    "metadata": {"name": name, "namespace": namespace}, "spec": spec}
        for kind in [None, "ClusterIP"]:
            self.create(core(namespace, "services"), service("internal-default" if kind is None else "internal", kind))
            cases.append({"kind": "Service", "type": kind or "default-ClusterIP", "status": 201})
        for kind in ["LoadBalancer", "NodePort"]:
            code = self.denied("POST", core(namespace, "services"), service(kind.lower(), kind),
                               "kars-no-public-router-exposure", "LoadBalancer/NodePort", 403)
            cases.append({"kind": "Service", "type": kind, "status": code})
        ingress = {"apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
                   "metadata": {"name": "public-ingress", "namespace": namespace},
                   "spec": {"rules": [{"http": {"paths": [{"path": "/", "pathType": "Prefix",
                             "backend": {"service": {"name": "internal", "port": {"number": 8443}}}}]}}]}}
        code = self.denied("POST", resource(namespace, "ingresses", group=NETWORK), ingress,
                           "kars-no-public-router-exposure", "forbid ingress objects", 403)
        cases.append({"kind": "Ingress", "status": code})
        def network(name, peer):
            return {"apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
                    "metadata": {"name": name, "namespace": namespace},
                    "spec": {"podSelector": {"matchLabels": {"app": "native-fixture"}},
                             "policyTypes": ["Ingress"], "ingress": [{"from": [peer]}]}}
        self.create(resource(namespace, "networkpolicies", group=NETWORK),
                    network("private-peers", {"podSelector": {"matchLabels": {"app": "native-peer"}}}))
        cases.append({"kind": "NetworkPolicy", "peer": "private-selector", "status": 201})
        for name, cidr in [("public-v4", "0.0.0.0/0"), ("public-v6", "::/0")]:
            code = self.denied("POST", resource(namespace, "networkpolicies", group=NETWORK),
                               network(name, {"ipBlock": {"cidr": cidr}}),
                               "kars-no-public-router-exposure", "forbidden in sandbox namespaces", 403)
            cases.append({"kind": "NetworkPolicy", "peer": cidr, "status": code})
        self.results["publicExposure"] = cases

    def cleanup(self):
        for path in reversed(self.created):
            if self.api.optional(path) is not None:
                self.api.delete(path)


def run_cases():
    cases = Cases()
    results = {}
    try:
        for name, operation in [("stores", cases.stores), ("rebind", cases.rebind), ("exposure", cases.exposure)]:
            try:
                operation()
                results[name] = {"result": "passed"}
            except Exception as error:
                results[name] = {"result": "failed",
                                 "failure": str(error) if isinstance(error, Failure) else type(error).__name__}
    finally:
        try:
            cases.cleanup()
            results["cleanup"] = {"result": "passed"}
        except Exception as error:
            results["cleanup"] = {"result": "failed",
                                  "failure": str(error) if isinstance(error, Failure) else type(error).__name__}
    cases.results["cases"] = results
    cases.results["result"] = "passed" if all(value["result"] == "passed" for value in results.values()) else "failed"
    return cases.results
