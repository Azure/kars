"""Fresh no-registration TLS proofs and real CNI paths, with no legacy fallback."""

import base64
import copy
import http.client
import json
import secrets
import ssl
import time

from lifecycle_cases import running
from credential_cases import SOURCE, selection
from enrollment import enroll
from native_api import BRIDGE, CORE, WRITER, command, core, private_file, require, resource, uid, until
from private_tls import call, forward
from runtime_state import runtime_state

CAPABILITY = "kars.azure.com/observation-privacy/v1"
PATH = "/internal/observations/verify-privacy"
OBSERVER = "router-services-observer"
NETWORK = "/apis/networking.k8s.io/v1"


class ObservationCases:
    def __init__(self, setup, bff, lifecycle):
        self.setup, self.bff, self.lifecycle = setup, bff, lifecycle
        self.observer_target = None

    def target(self):
        require(self.observer_target is not None, "Independent native observation Task is not ready")
        return self.observer_target

    def prepare_target(self):
        grant = self.setup.ready_grant(CORE)
        source = self.setup.admin.get(core(CORE, "secrets", SOURCE))
        task = self.setup.admin.create(resource(CORE, "karstasks"), {
            "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTask",
            "metadata": {"name": "native-observation-task", "namespace": CORE,
                         "annotations": {"kars.azure.com/owner-sub": "native-operator"}},
            "spec": {"objective": "Independent native observation qualification",
                     "envelope": {"tier": 1, "authorityCeiling": 1, "delegationDepth": 0},
                     "execution": {"launch": True},
                     "blueprint": {"runtime": "OpenClaw", "isolation": "standard",
                                   "model": {"provider": "azure-openai", "deployment": "native"},
                                   "credentialBindings": selection(grant, source)}},
        })
        def ready():
            current = self.setup.admin.get(resource(CORE, "karstasks", task["metadata"]["name"]))
            require(uid(current) == uid(task), "Observation Task was recreated")
            status = current.get("status", {})
            return current if status.get("phase") == "Ready" and status.get("sandboxRef", {}).get("name") else None
        current = until("independent real observation Task authority", ready, 180)
        self.observer_target = {"task": task["metadata"]["name"],
                                "sandbox": current["status"]["sandboxRef"]["name"], "workspace": CORE}
        value, _, _ = running(self.setup, CORE, self.observer_target["sandbox"])
        self.observer_target["uid"] = uid(value)

    def public(self):
        target = self.target()
        return self.bff.call("GET", f"/api/namespaces/{CORE}/tasks/{target['task']}/egress/learned")

    def enable(self):
        self.prepare_target()
        target = self.target()
        value, _, _ = running(self.setup, CORE, target["sandbox"])
        unavailable = self.public()
        require(unavailable.get("available") is False
                and "unavailable" in unavailable.get("reason", "").lower(),
                "Unenrolled observation did not clearly report capability unavailable")
        # A pre-existing, test-owned isolation baseline precedes the private
        # chart's additive path. This never changes production network policy.
        # Cilium represents the host-network API server by its reserved entity;
        # CIDR-only Kubernetes rules do not reliably match that identity.
        self.setup.admin.create(resource(BRIDGE, "ciliumnetworkpolicies", group="/apis/cilium.io/v2"), {
            "apiVersion": "cilium.io/v2", "kind": "CiliumNetworkPolicy",
            "metadata": {"name": "native-bff-api-baseline", "namespace": BRIDGE},
            "spec": {"endpointSelector": {"matchLabels": {
                "app.kubernetes.io/name": "kars-bridge", "app.kubernetes.io/component": "bff"}},
                "egress": [
                    {"toEntities": ["kube-apiserver"], "toPorts": [{"ports": [
                        {"port": "443", "protocol": "TCP"}, {"port": "6443", "protocol": "TCP"}]}]},
                    {"toEndpoints": [{"matchLabels": {
                        "k8s:io.kubernetes.pod.namespace": "kube-system", "k8s:k8s-app": "kube-dns"}}],
                     "toPorts": [{"ports": [{"port": "53", "protocol": "UDP"},
                                            {"port": "53", "protocol": "TCP"}]}]},
                ]},
        })
        self.setup.admin.create(resource(BRIDGE, "networkpolicies", group=NETWORK), {
            "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
            "metadata": {"name": "native-bff-baseline", "namespace": BRIDGE},
            "spec": {"podSelector": {"matchLabels": {
                "app.kubernetes.io/name": "kars-bridge", "app.kubernetes.io/component": "bff"}},
                "policyTypes": ["Egress"], "egress": [
                    {"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}},
                             "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}}}],
                     "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]},
                ]},
        })
        values = {"namespace": BRIDGE, "networkPolicy": {"observations": {
            "enabled": True, "existingIsolationConfirmed": True,
            "targetNamespaces": [f"kars-{target['sandbox']}"],
        }}}
        private_file("observation-values.json", json.dumps(values))
        manifest = command(
            "helm", "template", "bridge-native", "deploy/helm/kars-bridge", "--namespace", BRIDGE,
            "--values", ".native/observation-values.json", "--show-only", "templates/observation-egress.yaml",
        )
        command("kubectl", "apply", "-f", "-", stdin=manifest)
        until("BFF retains API connectivity under existing Cilium isolation", self.bff.ready, 30)
        grant = self.setup.ready_grant(CORE)
        writer = self.setup.admin.get(core(BRIDGE, "serviceaccounts", WRITER))
        enroll(self.setup, CORE, writer, grant["spec"]["agentKeys"], previous=grant, observations=[
            {"kind": "KarsSandbox", "namespace": CORE, "name": target["sandbox"], "uid": uid(value)},
        ])
        self.ready()
        until("real BFF-to-observer9447 and router-to-verifier9448", lambda:
              self.public().get("available") is True, 240)

    def ready(self):
        target = self.target()
        def ready():
            value = self.setup.admin.get(resource(CORE, "karssandboxes", target["sandbox"]))
            return value if value.get("status", {}).get("serviceObservation", {}).get("phase") == "Ready" else None
        value = until("core-issued current observer capability", ready, 240)
        self.setup.ready_grant(CORE)
        return value

    def material(self):
        target = self.target()
        value = self.ready()
        actor = self.setup.actor(BRIDGE, WRITER)
        secret = actor.get(core(f"kars-{target['sandbox']}", "secrets", OBSERVER))
        require(uid(secret) == value["status"]["serviceObservation"]["secret"]["uid"],
                "Native recipient did not receive the current canonical observer Secret")
        return value, secret, json.loads(base64.b64decode(secret["data"]["config.json"])), (
            base64.b64decode(secret["data"]["observation-token"]).decode()
        )

    def tls_api(self):
        target = self.target()
        value, secret, binding, token = self.material()
        _, _, pod = running(self.setup, CORE, target["sandbox"])
        with forward(f"kars-{target['sandbox']}", f"pod/{pod['metadata']['name']}", 19447, 9447), (
            forward(CORE, "service/kars-observation-privacy", 19448, 9448)
        ):
            status, scope = call(19447, binding, token, "GET", "/internal/observations/scope")
            require(status == 200 and scope["privacy_verifier"] == CAPABILITY,
                    "Scope did not perform the approved fresh core verifier protocol")
            request = {
                "capability": CAPABILITY, "purpose": "read-only-observation-privacy",
                "target": {"workspace": CORE, "workspaceUid": binding["workspaceUid"],
                           "name": target["sandbox"], "uid": uid(value),
                           "namespaceUid": value["status"]["serviceObservation"]["namespaceUid"]},
                "grantUid": binding["grant"]["uid"], "grantGeneration": binding["grant"]["generation"],
                "recipients": binding["recipients"], "credentialVersion": f"{uid(secret)}:{secret['metadata']['resourceVersion']}",
                "identity": binding["identity"], "scopeId": scope["scope_id"], "operation": "learned",
                "epoch": binding.get("privacyEpoch"), "nonce": secrets.token_hex(32), "verifier": binding["verifier"],
            }
            require(request["epoch"] is None, "Initial TLS lane unexpectedly acquired SRE registration")
            status, proof = call(19448, binding["verifier"], token, "POST", PATH, request)
            require(status == 200 and proof.get("allowed") is True
                    and proof["nonce"] == request["nonce"] and proof["epoch"] is None,
                    "Full real no-registration privacy proof failed")
            require(set(proof) == {"capability", "purpose", "allowed", "requestDigest", "nonce", "epoch"},
                    "Privacy response exposed more than bounded proof metadata")
            mutations = [
                ("target", "uid", "different-sandbox-uid"),
                ("target", "namespaceUid", "different-namespace-uid"),
                ("target", "workspaceUid", "different-workspace-uid"),
                (None, "grantUid", "different-grant-uid"),
                (None, "grantGeneration", binding["grant"]["generation"] + 1),
                (None, "credentialVersion", "different-source-version"),
                (None, "purpose", "operator"),
                (None, "nonce", "malformed"),
                (None, "epoch", "unregistered-fabricated-epoch"),
                ("verifier", "expiresAt", int(time.time()) - 1),
                ("verifier", "controllerUid", "different-controller-uid"),
            ]
            requests = []
            for parent, key, replacement in mutations:
                invalid = copy.deepcopy(request)
                (invalid[parent] if parent else invalid)[key] = replacement
                requests.append(invalid)
            invalid = copy.deepcopy(request)
            invalid["recipients"][0]["uid"] = "different-recipient-uid"
            requests.append(invalid)
            invalid = copy.deepcopy(request)
            invalid["sourceSecretName"] = "arbitrary-private-secret"
            requests.append(invalid)
            for invalid in requests:
                status, rejected = call(19448, binding["verifier"], token, "POST", PATH, invalid)
                require(status == 403 and rejected.get("allowed") is False,
                        "Wrong target/grant/recipient/purpose/nonce/version/epoch was authorized")
                require(set(rejected) == {"capability", "allowed"}, "Denial exposed an authority oracle")
            for method, path in [
                ("GET", "/api/v1/namespaces/kars-system/secrets"),
                ("POST", "/internal/egress/reset"), ("POST", "/token"),
                ("POST", PATH + "?source=arbitrary"), ("DELETE", "/internal/observations/scope"),
            ]:
                status, _ = call(19448, binding["verifier"], token, method, path, request)
                require(status in (403, 405), "Purpose credential reached an RPC mutation or proxy route")
            status, _ = call(19447, binding, token, "GET",
                             "/internal/observations/egress/learned", scope="wrong-current-scope")
            require(status == 409, "Observer accepted a different local request scope")
            for path in ["/internal/egress/reset", "/internal/services/control", "/api/github/token"]:
                status, _ = call(19447, binding, token, "POST", path)
                require(status in (403, 404, 405, 410), "Observation purpose authorized a mutation")
            with forward(f"kars-{target['sandbox']}", f"pod/{pod['metadata']['name']}", 18443, 8443):
                for method, path in [("GET", "/egress/learned"), ("POST", "/egress/learned/clear")]:
                    connection = http.client.HTTPConnection("127.0.0.1", 18443, timeout=15)
                    try:
                        connection.request(method, path, headers={"Authorization": f"Bearer {token}"})
                        response = connection.getresponse()
                        response.read(65536)
                        require(response.status == 403,
                                "Purpose-only token authorized a real legacy route even over loopback")
                    finally:
                        connection.close()
            wrong_tls = dict(binding["verifier"], serverName=binding["serverName"])
            try:
                call(19448, wrong_tls, token, "POST", PATH, request)
            except ssl.SSLCertVerificationError:
                pass
            else:
                raise AssertionError("UID hostname pinning was not enforced")
            # Remove a required admission binding. A positive proof issued
            # moments ago cannot authorize another request after this change.
            admission_path = "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicybindings/kars-observation-privacy-material"
            admission = self.setup.admin.get(admission_path)
            self.setup.admin.delete(admission_path)
            try:
                status, denied = call(19448, binding["verifier"], token, "POST", PATH, request)
                require(status == 403 and denied.get("allowed") is False,
                        "A cached positive proof survived loss of actual admission")
            finally:
                admission["metadata"] = {"name": admission["metadata"]["name"]}
                self.setup.admin.create("/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicybindings", admission)

    def cni_denials(self):
        target = self.target()
        until("current public observer after admission restoration",
              lambda: self.public().get("available") is True, 240)
        _, _, pod = running(self.setup, CORE, target["sandbox"])
        verifier_service = self.setup.admin.get(core(CORE, "services", "kars-observation-privacy"))
        runtime = f"kars-{target['sandbox']}"
        agent = self.setup.actor(runtime, pod["spec"]["serviceAccountName"])
        agent.request("GET", core(runtime, "secrets", OBSERVER), expected=(403,))
        agent.request("GET", core(CORE, "secrets", "kars-observation-privacy-tls"), expected=(403,))
        require(runtime_state(runtime, pod["metadata"]["name"])["observationFilesUnreadable"],
                "The protected agent could read private observation material")
        self.setup.namespace("native-untrusted")
        self.setup.admin.create(core("native-untrusted", "pods"), {
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "network-peer", "namespace": "native-untrusted"},
            "spec": {"automountServiceAccountToken": False,
                     "securityContext": {"runAsUser": 10001, "runAsNonRoot": True,
                                         "seccompProfile": {"type": "RuntimeDefault"}},
                     "containers": [{"name": "peer", "image": "docker.io/library/kars-native-probe:latest",
                                     "imagePullPolicy": "IfNotPresent",
                                     "securityContext": {"allowPrivilegeEscalation": False,
                                                         "capabilities": {"drop": ["ALL"]}}}]},
        })
        command("kubectl", "wait", "-n", "native-untrusted", "--for=condition=Ready",
                "pod/network-peer", "--timeout=120s", timeout=135)
        targets = {
            "control": [self.setup.admin.get(core("default", "services", "kubernetes"))["spec"]["clusterIP"], 443],
            "denied": [[pod["status"]["podIP"], 9447], [verifier_service["spec"]["clusterIP"], 9448]],
        }
        script = """import json,socket,sys
targets=json.load(sys.stdin)
socket.create_connection(tuple(targets["control"]),3).close()
for host,port in targets["denied"]:
    try:
        connection=socket.create_connection((host,port),3)
    except (TimeoutError,OSError):
        continue
    connection.close()
    sys.exit(1)
print("unauthorized-peer-tcp-denied")
"""
        result = command("kubectl", "exec", "-i", "-n", "native-untrusted", "network-peer", "--",
                         "python3", "-c", script, stdin=json.dumps(targets), timeout=30)
        require(result.strip() == "unauthorized-peer-tcp-denied",
                "Real CNI did not deny unauthorized peer TCP9447/9448")
        require(self.public().get("available") is True,
                "Negative CNI test only passed because the authorized service was unavailable")

    def rotation(self):
        value, old, binding, token = self.material()
        target = self.target()
        self.setup.admin.patch(resource(CORE, "karscredentialgrants", "workspace"),
                               {"spec": {"agentKeys": ["SLACK_BOT_TOKEN", "TELEGRAM_BOT_TOKEN", "BRAVE_API_KEY"]}})
        until("rotated source revision acknowledged", lambda:
              (self.ready()["status"]["serviceObservation"]["version"]
               != f"{uid(old)}:{old['metadata']['resourceVersion']}"), 240)
        _, fresh, new_binding, new_token = self.material()
        require(new_token != token and new_binding["grant"]["generation"] > binding["grant"]["generation"],
                "Grant generation did not rotate actual observer credential authority")
        _, _, pod = running(self.setup, CORE, target["sandbox"])
        with forward(f"kars-{target['sandbox']}", f"pod/{pod['metadata']['name']}", 19447, 9447):
            status, _ = call(19447, new_binding, token, "GET", "/internal/observations/scope")
            require(status == 403, "Rotated observer consumer retained the old bearer")
            status, scope = call(19447, new_binding, new_token, "GET", "/internal/observations/scope")
            require(status == 200 and scope["privacy_verifier"] == CAPABILITY,
                    "New observer bearer did not perform a fresh current proof")
