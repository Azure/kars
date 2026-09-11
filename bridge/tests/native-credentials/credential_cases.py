"""Public BFF entrypoints under actual native writer RBAC and API audit."""

import base64
import copy
import secrets

from native_api import BRIDGE, CORE, WRITER, core, require, resource, uid, until

SOURCE = "kars-credential-input-workspace"
KEY = "SLACK_BOT_TOKEN"
MUTATIONS = {"create", "patch", "update", "delete", "deletecollection"}


def selection(grant, source, scope="workspace", owner=None, keys=None):
    value = {"scope": scope, "source": {"name": source["metadata"]["name"], "uid": uid(source)},
             "keys": keys or [KEY]}
    if owner:
        value["owner"] = owner
    return {"grant": {"name": "workspace", "uid": uid(grant)}, "sources": [value]}


def inference(setup, namespace, name):
    policy_path = resource(namespace, "toolpolicies", "kars-default")
    if setup.admin.optional(policy_path) is None:
        policy = setup.admin.get(resource(CORE, "toolpolicies", "kars-default"))
        setup.admin.create(resource(namespace, "toolpolicies"), {
            "apiVersion": "kars.azure.com/v1alpha1", "kind": "ToolPolicy",
            "metadata": {"name": "kars-default", "namespace": namespace},
            "spec": policy["spec"],
        })
    return setup.admin.create(resource(namespace, "inferencepolicies"), {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "InferencePolicy",
        "metadata": {"name": name, "namespace": namespace},
        "spec": {"appliesTo": {"sandboxName": name},
                 "modelPreference": {"primary": {"provider": "azure-openai", "deployment": "native"}}},
    })


def sandbox(setup, namespace, name, bindings=None, legacy=None, suspended=True):
    inference(setup, namespace, name)
    spec = {"runtime": {"kind": "OpenClaw", "openclaw": {}},
            "sandbox": {"isolation": "standard"}, "inferenceRef": {"name": name},
            "suspended": suspended}
    if bindings:
        spec["credentialBindings"] = bindings
    if legacy:
        spec["credentialsRef"] = legacy
    return setup.admin.create(resource(namespace, "karssandboxes"), {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSandbox",
        "metadata": {"name": name, "namespace": namespace}, "spec": spec,
    })


def paused_team(setup, namespace, name):
    return setup.admin.create(resource(namespace, "karsteams"), {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTeam",
        "metadata": {"name": name, "namespace": namespace},
        "spec": {"charter": "Native credential qualification", "paused": True, "blueprint": {},
                 "envelope": {"tier": 1, "authorityCeiling": 1, "delegationDepth": 0}},
    })


def changes_since(setup, actor, namespace, before):
    ids = {event["auditID"] for event in before}
    return [event for event in setup.audit_barrier(actor, namespace) if event["auditID"] not in ids]


class CredentialCases:
    def __init__(self, setup, bff):
        self.setup = setup
        self.bff = bff
        self.writer = setup.admin.get(core(BRIDGE, "serviceaccounts", WRITER))
        self.actor = setup.actor(BRIDGE, WRITER)

    def workspace(self, namespace):
        self.setup.namespace(namespace)
        return self.setup.grant(namespace, self.writer)

    def bootstrap(self):
        namespace = "native-bootstrap"
        grant = self.workspace(namespace)
        team = paused_team(self.setup, namespace, "bootstrap")
        v1_source = self.setup.admin.create(core(namespace, "secrets"), {
            "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
            "metadata": {"name": "kars-credential-source-native-v1", "namespace": namespace,
                         "annotations": {"kars.azure.com/credential-purpose": "agent-source-v1",
                                         "kars.azure.com/credential-target": "native-v1",
                                         "kars.azure.com/credential-workspace": namespace,
                                         "kars.azure.com/credential-binding-intent": "explicit-reference-v1"}},
            "data": {KEY: base64.b64encode(secrets.token_hex(24).encode()).decode()},
        })
        legacy = sandbox(self.setup, namespace, "native-v1",
                         legacy={"name": v1_source["metadata"]["name"], "uid": uid(v1_source)})
        self.actor.request("GET", core(namespace, "secrets", SOURCE), expected=(403,))
        self.actor.request("GET", core(namespace, "secrets"), expected=(403,))
        before = self.setup.audit_barrier(self.actor, namespace)
        value = secrets.token_hex(24)
        result = self.bff.channel(namespace, value)
        require("slack" in result.get("enabled", []), "Workspace channel was not reported stored")
        source = self.setup.admin.get(core(namespace, "secrets", SOURCE))
        require(base64.b64decode(source["data"][KEY]).decode() == value,
                "Native source did not retain the requested value")
        observed = self.setup.ready_grant(namespace)
        require(any(item["name"] == SOURCE and item["uid"] == uid(source)
                    for item in observed["status"]["sources"]), "Core did not acknowledge CREATE UID")
        self.actor.get(core(namespace, "secrets", SOURCE))
        current = self.setup.admin.get(resource(namespace, "karsteams", "bootstrap"))
        binding = current["spec"]["blueprint"]["credentialBindings"]
        require(binding["grant"]["uid"] == uid(grant)
                and binding["sources"][0]["source"]["uid"] == uid(source),
                "Team did not bind the actual created source UID")
        require(uid(current) == uid(team), "Workspace authoring recreated a Team")
        current_legacy = self.setup.admin.get(resource(namespace, "karssandboxes", "native-v1"))
        require(uid(current_legacy) == uid(legacy)
                and current_legacy["spec"]["credentialsRef"] == legacy["spec"]["credentialsRef"]
                and "credentialBindings" not in current_legacy["spec"],
                "Workspace authoring converted a v1 consumer without explicit migration")
        events = changes_since(self.setup, self.actor, namespace, before)
        posts = [event for event in events if event["verb"] == "create"
                 and event.get("objectRef", {}).get("resource") == "secrets"
                 and event.get("responseStatus", {}).get("code") == 201]
        require(len(posts) == 1, "Bootstrap did not use one exclusive native source CREATE")
        created_at = posts[0]["requestReceivedTimestamp"]
        gets = [event for event in events if event["verb"] == "get"
                and event.get("objectRef", {}).get("resource") == "secrets"
                and event.get("objectRef", {}).get("name") == SOURCE]
        require(gets and all(event["requestReceivedTimestamp"] > created_at for event in gets),
                "BFF attempted native source GET before exclusive CREATE")
        first_get = min(event["requestReceivedTimestamp"] for event in gets)
        require(any(event["verb"] == "get"
                    and event.get("objectRef", {}).get("resource") == "karscredentialgrants"
                    and created_at < event["requestReceivedTimestamp"] < first_get
                    for event in events), "No metadata acknowledgement precedes native source read")
        return namespace

    def collision(self):
        namespace = "native-collision"
        self.workspace(namespace)
        paused_team(self.setup, namespace, "collision-survivor")
        existing = self.setup.admin.create(core(namespace, "secrets"), {
            "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
            "metadata": {"name": SOURCE, "namespace": namespace, "annotations": {
                "kars.azure.com/credential-purpose": "agent-input-v2",
                "kars.azure.com/credential-workspace": namespace,
                "kars.azure.com/credential-binding-intent": "explicit-reference-v2",
                "kars.azure.com/credential-target-kind": "Workspace",
                "kars.azure.com/credential-target": namespace,
                "kars.azure.com/credential-grant-uid": "foreign-grant-uid",
            }},
            "data": {KEY: base64.b64encode(secrets.token_hex(24).encode()).decode()},
        })
        before_team = self.setup.admin.get(resource(namespace, "karsteams", "collision-survivor"))
        self.actor.request("GET", core(namespace, "secrets", SOURCE), expected=(403,))
        before = self.setup.audit_barrier(self.actor, namespace)
        self.bff.channel(namespace, secrets.token_hex(24), expected=502)
        after = self.setup.admin.get(core(namespace, "secrets", SOURCE))
        after_team = self.setup.admin.get(resource(namespace, "karsteams", "collision-survivor"))
        require(uid(after) == uid(existing) and after["data"] == existing["data"],
                "Unobserved source collision changed values or adopted a replacement")
        require(uid(after_team) == uid(before_team) and after_team["spec"] == before_team["spec"],
                "Unobserved source collision changed a consumer")
        events = changes_since(self.setup, self.actor, namespace, before)
        mutations = [event for event in events if event["verb"] in MUTATIONS]
        require(len(mutations) == 1 and mutations[0]["verb"] == "create"
                and mutations[0].get("responseStatus", {}).get("code") == 409,
                "Collision was not one exclusive CREATE409 with no fallback mutations")
        require(not any(event["verb"] == "get"
                        and event.get("objectRef", {}).get("resource") == "secrets"
                        for event in events), "Collision performed an unauthorized adoption GET")

    def late_conflict(self, existing):
        namespace = "native-conflict-existing" if existing else "native-conflict-new"
        grant = self.workspace(namespace)
        if existing:
            self.bff.channel(namespace, secrets.token_hex(24))
            self.setup.ready_grant(namespace)
        team = paused_team(self.setup, namespace, "first-consumer")
        foreign = {"grant": {"name": "workspace", "uid": "foreign-grant-uid"}, "sources": [{
            "scope": "workspace", "source": {"name": SOURCE, "uid": "foreign-source-uid"}, "keys": [KEY],
        }]}
        late = sandbox(self.setup, namespace, "native-late-" + ("old" if existing else "new"), foreign)
        before_source = self.setup.admin.optional(core(namespace, "secrets", SOURCE))
        snapshots = [
            (resource(namespace, "karsteams", team["metadata"]["name"]), uid(team), copy.deepcopy(team["spec"])),
            (resource(namespace, "karssandboxes", late["metadata"]["name"]), uid(late), copy.deepcopy(late["spec"])),
        ]
        self.setup.ready_grant(namespace)
        before = self.setup.audit_barrier(self.actor, namespace)
        self.bff.channel(namespace, secrets.token_hex(24), expected=502)
        after_source = self.setup.admin.optional(core(namespace, "secrets", SOURCE))
        require((before_source is None and after_source is None) or (
            before_source is not None and after_source is not None
            and uid(before_source) == uid(after_source)
            and before_source.get("data") == after_source.get("data")
        ), "Late consumer conflict mutated source values or UID")
        for path, identity, spec in snapshots:
            current = self.setup.admin.get(path)
            require(uid(current) == identity and current["spec"] == spec,
                    "Late consumer conflict partially converted earlier consumers")
        events = changes_since(self.setup, self.actor, namespace, before)
        require(not any(event["verb"] in MUTATIONS for event in events),
                "Public BFF entrypoint mutated native API before complete consumer preflight")
        require(uid(self.setup.ready_grant(namespace)) == uid(grant),
                "Late conflict replaced the workspace grant")

    def native_denials(self, namespace):
        self.setup.account(BRIDGE, "unregistered")
        self.setup.namespace("bridge-native-alias")
        self.setup.account("bridge-native-alias", WRITER)
        for actor in [self.setup.actor(BRIDGE, "unregistered"),
                      self.setup.actor("bridge-native-alias", WRITER)]:
            actor.request("GET", core(namespace, "secrets", SOURCE), expected=(403,))
            actor.request("GET", core(namespace, "secrets"), expected=(403,))
            actor.request("POST", core(namespace, "secrets"), {
                "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
                "metadata": {"name": "kars-credential-input-workspace", "namespace": namespace},
            }, expected=(403,))
        self.actor.request("GET", core(BRIDGE, "secrets", "native-principal"), expected=(403,))
        self.actor.request("GET", core(namespace, "secrets") + "?watch=true&timeoutSeconds=1",
                           expected=(403,))
        self.actor.request("POST", resource(namespace, "roles", group="/apis/rbac.authorization.k8s.io/v1"), {
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
            "metadata": {"name": "reader-alias", "namespace": namespace},
            "rules": [{"apiGroups": [""], "resources": ["secrets"], "verbs": ["get"]}],
        }, expected=(403,))
        self.actor.request("POST", resource(namespace, "rolebindings", group="/apis/rbac.authorization.k8s.io/v1"), {
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
            "metadata": {"name": "reader-alias", "namespace": namespace},
            "subjects": [{"kind": "ServiceAccount", "name": WRITER, "namespace": BRIDGE}],
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role",
                        "name": "kars-credential-writer-alias"},
        }, expected=(403,))

    def removal_before_import(self):
        namespace = "native-removal"
        self.setup.namespace(namespace)
        legacy = self.setup.admin.create(core(namespace, "secrets"), {
            "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
            "metadata": {"name": "kars-workspace-channels", "namespace": namespace},
            "data": {KEY: base64.b64encode(secrets.token_hex(24).encode()).decode()},
        })
        self.setup.grant(namespace, self.writer)
        grant = until("native metadata-only legacy inventory",
                      lambda: (value if (value := self.setup.ready_grant(namespace))
                               .get("status", {}).get("legacySources") else None))
        imports = grant["status"]["legacySources"]
        require(any(item["secret"]["uid"] == uid(legacy) for item in imports),
                "Legacy inventory did not pin actual native Secret UID")
        # Explicit operator review of this synthetic legacy source, before
        # creating its new source. A channel save is never migration approval.
        self.setup.admin.patch(resource(namespace, "karscredentialgrants", "workspace"),
                               {"spec": {"legacyImports": imports}})
        self.setup.ready_grant(namespace)
        self.bff.call("DELETE", f"/api/namespaces/{namespace}/channels/slack")
        source = self.setup.admin.get(core(namespace, "secrets", SOURCE))
        removed = json_removed(source)
        require(KEY not in source.get("data", {}) and KEY in removed,
                "First source CREATE lost persistent pre-import removal intent")
        self.bff.channel(namespace, secrets.token_hex(24), channel="telegram")
        source_after = self.setup.admin.get(core(namespace, "secrets", SOURCE))
        old_after = self.setup.admin.get(core(namespace, "secrets", "kars-workspace-channels"))
        require(uid(source_after) == uid(source) and KEY not in source_after.get("data", {})
                and KEY in json_removed(source_after),
                "Later native import or source update resurrected a removed credential")
        require(uid(old_after) == uid(legacy) and old_after["data"] == legacy["data"],
                "Explicit legacy review mutated the preserved original source")


def json_removed(source):
    import json
    return json.loads(source["metadata"].get("annotations", {}).get(
        "kars.azure.com/credential-removed-keys", "[]"))
