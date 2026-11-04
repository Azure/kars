# Copyright (c) Microsoft Corporation. Licensed under the MIT License.
"""Actual Helm caller/profile checks. All identities, endpoints and APIs are offline fixtures."""
import base64
import copy
import json
from pathlib import Path
import tempfile
import unittest

import yaml
from test_chart import ROOT, RESOURCES, FakeApi, documents, render

CORE = ROOT / "deploy/helm/kars"
BRIDGE = ROOT / "bridge/deploy/helm/kars-bridge"
DIGEST = "sha256:" + "a" * 64
IMAGE = "example.invalid/qualified:latest@" + DIGEST


def core_values():
    image = {"repository": "example.invalid/qualified", "tag": "latest@" + DIGEST}
    return {
        "kindBeta": {"cni": "calico"},
        "monitoring": {"enabled": False}, "metricsServer": {"enabled": False},
        "schemaHook": {"image": {"repository": "example.invalid/helper", "digest": DIGEST},
                       "apiDestinations": [{"cidr": "10.96.0.1/32", "port": 443}]},
        "controller": {"image": dict(image, digest=DIGEST)},
        "sandbox": {"image": image, "seccompInstallerImage": IMAGE,
                    "nodeSelector": {"kubernetes.io/os": "linux", "kubernetes.io/hostname": "kind-worker"}},
        "runtimes": {"hermes": {"image": IMAGE}},
        "inferenceRouter": {"image": image, "azure": {"openai": {
            "endpoint": "https://provider.example.invalid", "credentials": {"existingSecret": "provider-key"}}}},
        "agentMesh": {"registry": {"image": image}, "relay": {"image": image}},
        "headlamp": {"image": image, "oidc": {
            "issuerURL": "https://issuer.example.invalid", "clientID": "headlamp-client",
            "clientSecret": {"name": "headlamp-oidc"}, "callbackURL": "http://localhost:3903/oidc-callback"}},
        "sre": {"provider": "azure-openai", "model": "operator-selected-model",
                "firstInstallFunding": {"unboundedTokens": True}},
        "inferenceBudget": {"routerImageDigest": DIGEST, "catalogVersion": "offline-v1",
                            "caBundle": "offline-public-ca", "tlsSecretName": "budget-tls",
                            "contracts": [{"id": "offline-render-contract"}]},
        "datapathWitness": {"nodes": ["kind-worker"], "aggregator": {"image": IMAGE},
                           "gadget": {"image": IMAGE, "hookMode": "podinformer"}},
    }


def bridge_values():
    return {"web": {"image": {"repository": "example.invalid/web", "tag": "latest@" + DIGEST}, "extraEnv": [
        {"name": "BRIDGE_OIDC_ISSUER", "value": "https://issuer.example.invalid"},
        {"name": "BRIDGE_OIDC_CLIENT_ID", "value": "bridge-client"},
        {"name": "BRIDGE_OIDC_REDIRECT_URI", "value": "http://localhost:3000/auth/callback"},
        {"name": "BRIDGE_OIDC_ROLE_CLAIM", "value": "groups"},
        {"name": "BRIDGE_OIDC_ROLE_MAP", "value": '{"real-operator-group":"operator"}'},
        {"name": "BRIDGE_OIDC_CLIENT_SECRET", "valueFrom": {"secretKeyRef": {"name": "bridge-oidc", "key": "client-secret"}}},
    ]}, "bff": {"image": {"repository": "example.invalid/bff", "tag": "latest@" + DIGEST}},
        "auth": {"principalSecretName": "bridge-session"}, "headlamp": {"url": "http://localhost:3903"}}


def profile(chart, values, *extra, **kwargs):
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "operator.yaml"
        path.write_text(yaml.safe_dump(values))
        base = ["--values", str(CORE / "values-generic.yaml")] if chart == CORE else []
        return render(*base, "--values", str(chart / "values-kind-beta.yaml"), "--values", str(path),
                      "--kube-version", "1.35.0", "--api-versions", "crd.projectcalico.org/v1", *extra,
                      chart=chart, release="kars" if chart == CORE else "kars-bridge", **kwargs)


class HeadlampProjectionTests(unittest.TestCase):
    """Only synthetic source data is exposed to Helm's loopback lookup API."""

    def values(self, ca=True):
        oidc = copy.deepcopy(core_values()["headlamp"]["oidc"])
        oidc.update(enabled=True, clientSecret={"sourceName": "operator-oidc", "name": "headlamp-oidc",
                                                "key": "selected-key"})
        if ca:
            oidc.update(caSourceConfigMap="operator-ca", caConfigMap="headlamp-ca")
        return {"oidc": oidc}

    def sources(self):
        return [{"apiVersion": "v1", "kind": "Secret", "type": "Opaque",
                 "metadata": {"name": "operator-oidc", "namespace": "kars-system", "annotations": {"operator-only": "fixture"}},
                 "data": {"selected-key": base64.b64encode(b"synthetic-oidc-fixture").decode(),
                          "unselected-key": base64.b64encode(b"synthetic-do-not-copy").decode()}},
                {"apiVersion": "v1", "kind": "ConfigMap",
                 "metadata": {"name": "operator-ca", "namespace": "kars-system"},
                 "data": {"ca.crt": "-----BEGIN CERTIFICATE-----\nb2ZmbGluZS1maXh0dXJl\n-----END CERTIFICATE-----\n",
                          "unselected-key": "do-not-copy"}}]

    def api(self, objects=(), **kwargs):
        resources = dict(RESOURCES, **{
            "v1": RESOURCES["v1"] + [("secrets", "Secret", True), ("services", "Service", True)],
            "networking.k8s.io/v1": [("networkpolicies", "NetworkPolicy", True)],
            "admissionregistration.k8s.io/v1": [("validatingadmissionpolicies", "ValidatingAdmissionPolicy", False),
                                               ("validatingadmissionpolicybindings", "ValidatingAdmissionPolicyBinding", False)],
            "apiextensions.k8s.io/v1": [("customresourcedefinitions", "CustomResourceDefinition", False)],
            "kars.azure.com/v1alpha1": [("toolpolicies", "ToolPolicy", True)],
            "batch/v1": [("jobs", "Job", True)],
            "crd.projectcalico.org/v1": [("ippools", "IPPool", False)],
        })
        return FakeApi(objects, resources=resources, **kwargs)

    def render(self, values, *args, **kwargs):
        return render("--set", "metricsServer.enabled=false,monitoring.enabled=false",
                      "--set-json", "headlamp=" + json.dumps(values), *args,
                      chart=CORE, release="kars", **kwargs)

    def test_core_reuses_restricted_operator_namespace_without_adoption(self):
        namespace = {"apiVersion": "v1", "kind": "Namespace", "metadata": {
            "name": "kars-system", "uid": "operator-namespace-uid", "labels": {
                f"pod-security.kubernetes.io/{mode}": "restricted" for mode in ["enforce", "audit", "warn"]}}}
        namespace["metadata"]["labels"].update({"app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "system"})
        with self.api([namespace]) as api:
            docs = documents(self.render({"enabled": False}, kubeconfig=api.kubeconfig))
        self.assertFalse(any(o["kind"] == "Namespace" and o["metadata"]["name"] == "kars-system" for o in docs))
        namespace["metadata"]["labels"]["app.kubernetes.io/managed-by"] = "Helm"
        namespace["metadata"]["annotations"] = {"meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system"}
        with self.api([namespace]) as api:
            docs = documents(self.render({"enabled": False}, kubeconfig=api.kubeconfig))
        self.assertEqual(sum(o["kind"] == "Namespace" and o["metadata"]["name"] == "kars-system" for o in docs), 1)

    def test_core_requires_policy_confirmation_labels_on_operator_namespace(self):
        labels = {f"pod-security.kubernetes.io/{mode}": "restricted" for mode in ["enforce", "audit", "warn"]}
        labels.update({"app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "system"})
        for key in ["app.kubernetes.io/name", "app.kubernetes.io/component"]:
            for value in [None, "foreign"]:
                with self.subTest(key=key, value=value):
                    actual = dict(labels)
                    if value is None:
                        del actual[key]
                    else:
                        actual[key] = value
                    namespace = {"apiVersion": "v1", "kind": "Namespace", "metadata": {
                        "name": "kars-system", "labels": actual}}
                    with self.api([namespace]) as api:
                        result = self.render({"enabled": False}, kubeconfig=api.kubeconfig)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("for sandbox policy confirmation", result.stderr)

    def test_core_rejects_unsafe_or_conflicting_operator_namespace(self):
        base = {"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "kars-system", "labels": {
            f"pod-security.kubernetes.io/{mode}": "restricted" for mode in ["enforce", "audit", "warn"]}}}
        cases = [({"labels": {}}, "requires restricted Pod Security"),
                 ({"labels": {"pod-security.kubernetes.io/enforce": "baseline"}}, "requires restricted Pod Security"),
                 ({"deletionTimestamp": "2026-01-01T00:00:00Z"}, "must not be terminating"),
                 ({"annotations": {"meta.helm.sh/release-name": "another-release"}}, "conflicting or incomplete Helm ownership"),
                 ({"labels": {"app.kubernetes.io/managed-by": "Helm"}}, "conflicting or incomplete Helm ownership")]
        for metadata, error in cases:
            with self.subTest(metadata=metadata):
                namespace = copy.deepcopy(base)
                namespace["metadata"].update(metadata)
                with self.api([namespace]) as api:
                    result = self.render({"enabled": False}, kubeconfig=api.kubeconfig)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(error, result.stderr)

    def targets(self, docs):
        return [o for o in docs if o["kind"] in ("Secret", "ConfigMap") and o["metadata"].get("namespace") == "headlamp"]

    def assert_tls(self, docs, ca):
        pod = next(o for o in docs if o["kind"] == "Deployment" and o["metadata"]["name"] == "headlamp")["spec"]["template"]["spec"]
        container = pod["containers"][0]
        env = {e["name"]: e for e in container["env"]}
        self.assertEqual(env["HEADLAMP_CONFIG_OIDC_CLIENT_SECRET"]["valueFrom"]["secretKeyRef"],
                         {"name": "headlamp-oidc", "key": "selected-key"})
        self.assertEqual(env["HEADLAMP_CONFIG_OIDC_USE_PKCE"]["value"], "true")
        self.assertEqual(env["HEADLAMP_CONFIG_OIDC_SKIP_TLS_VERIFY"]["value"], "false")
        self.assertEqual(env["HEADLAMP_CONFIG_OIDC_IDP_ISSUER_URL"]["value"], "https://issuer.example.invalid")
        if ca:
            self.assertEqual(env["HEADLAMP_CONFIG_OIDC_CA_FILE"]["value"], "/etc/headlamp/oidc/ca.crt")
            self.assertEqual(next(v for v in pod["volumes"] if v["name"] == "oidc-ca")["configMap"],
                             {"name": "headlamp-ca", "defaultMode": 0o444, "items": [{"key": "ca.crt", "path": "ca.crt"}]})
            self.assertTrue(next(v for v in container["volumeMounts"] if v["name"] == "oidc-ca")["readOnly"])
        else:
            self.assertNotIn("HEADLAMP_CONFIG_OIDC_CA_FILE", env)
            self.assertNotIn("oidc-ca", [v["name"] for v in pod["volumes"]])

    def test_cold_projection_creates_owned_namespace_and_only_selected_data(self):
        sources = self.sources()
        with self.api(sources) as api:
            docs = documents(self.render(self.values(), kubeconfig=api.kubeconfig))
            data_calls = [p for p in api.calls if any(p.endswith("/" + name) for name in
                          ("operator-oidc", "operator-ca", "headlamp-oidc", "headlamp-ca"))]
            self.assertFalse(any(p.endswith(("/secrets", "/configmaps")) for p in api.calls))
            self.assertEqual(set(data_calls), {
                "/api/v1/namespaces/kars-system/secrets/operator-oidc",
                "/api/v1/namespaces/kars-system/configmaps/operator-ca",
                "/api/v1/namespaces/headlamp/secrets/headlamp-oidc",
                "/api/v1/namespaces/headlamp/configmaps/headlamp-ca"})
        namespace = next(o for o in docs if o["kind"] == "Namespace" and o["metadata"]["name"] == "headlamp")
        self.assertEqual(namespace["metadata"]["annotations"]["kars.azure.com/headlamp-owner"], "kars-system/kars")
        targets = self.targets(docs)
        self.assertEqual(len(targets), 2)
        for target in targets:
            source = next(o for o in sources if o["kind"] == target["kind"])
            key = "selected-key" if target["kind"] == "Secret" else "ca.crt"
            self.assertEqual(target["data"], {key: source["data"][key]})
            self.assertEqual(target["metadata"]["labels"]["app.kubernetes.io/managed-by"], "Helm")
            self.assertEqual(target["metadata"]["annotations"], {
                "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system"})
        self.assertEqual(next(o for o in targets if o["kind"] == "Secret")["type"], "Opaque")
        self.assertFalse(any(o["kind"] == "Job" for o in docs))
        self.assert_tls(docs, ca=True)
        # Helm stamps the namespace's release ownership when installing it.
        namespace["metadata"]["labels"]["app.kubernetes.io/managed-by"] = "Helm"
        namespace["metadata"]["annotations"].update(targets[0]["metadata"]["annotations"])
        # A subsequent Helm render accepts owned targets and refreshes only the selected value.
        sources[0]["data"]["selected-key"] = base64.b64encode(b"rotated-synthetic-fixture").decode()
        with self.api(sources + targets + [namespace]) as api:
            updated = self.targets(documents(self.render(self.values(), kubeconfig=api.kubeconfig)))
        self.assertEqual(next(o for o in updated if o["kind"] == "Secret")["data"],
                         {"selected-key": sources[0]["data"]["selected-key"]})

    def test_no_copy_defaults_and_optional_ca_preserve_tls(self):
        with self.api() as api:
            defaults = documents(self.render({}, kubeconfig=api.kubeconfig))
            self.assertEqual(self.targets(defaults), [])
            for ca in (False, True):
                values = self.values(ca)
                values["oidc"]["clientSecret"].pop("sourceName")
                values["oidc"].pop("caSourceConfigMap", None)
                docs = documents(self.render(values, kubeconfig=api.kubeconfig))
                self.assertEqual(self.targets(docs), [])
                self.assert_tls(docs, ca)
            self.assertFalse(any("/secrets" in p or "/namespaces/headlamp/configmaps" in p or
                                 p.endswith("/operator-ca") for p in api.calls))
        for mode in ("secret-only", "reference-ca", "ca-only"):
            with self.subTest(mode=mode), self.api(self.sources()) as api:
                values = self.values(ca=mode != "secret-only")
                if mode == "reference-ca":
                    values["oidc"].pop("caSourceConfigMap")
                if mode == "ca-only":
                    values["oidc"]["clientSecret"].pop("sourceName")
                docs = documents(self.render(values, kubeconfig=api.kubeconfig))
                self.assertEqual([o["kind"] for o in self.targets(docs)], ["ConfigMap" if mode == "ca-only" else "Secret"])
                self.assert_tls(docs, ca=mode != "secret-only")

    def test_sources_fail_closed_without_credential_output(self):
        cases = ["missing", "namespace", "terminating", "missing-data", "wrong-data-type", "missing-key",
                 "empty", "wrong-value-type", "invalid-value"]
        for kind in ("Secret", "ConfigMap"):
            for case in cases + (["wrong-secret-type", "invalid-secret-type", "missing-secret-type", "whitespace"] if kind == "Secret" else ["private-key"]):
                with self.subTest(kind=kind, case=case):
                    sources = self.sources()
                    source = next(o for o in sources if o["kind"] == kind)
                    key = "selected-key" if kind == "Secret" else "ca.crt"
                    if case == "missing":
                        sources.remove(source)
                    elif case == "namespace":
                        source["metadata"]["namespace"] = "unrelated"
                    elif case == "terminating":
                        source["metadata"]["deletionTimestamp"] = "2026-01-01T00:00:00Z"
                    elif case == "missing-data":
                        source.pop("data")
                    elif case == "wrong-data-type":
                        source["data"] = ["synthetic-sensitive-sentinel"]
                    elif case == "missing-key":
                        source["data"].pop(key)
                    elif case == "wrong-secret-type":
                        source["type"] = "kubernetes.io/tls"
                    elif case == "invalid-secret-type":
                        source["type"] = {"synthetic-sensitive-sentinel": True}
                    elif case == "missing-secret-type":
                        source.pop("type")
                    elif case == "whitespace":
                        source["data"][key] = base64.b64encode(b" \n").decode()
                    else:
                        source["data"][key] = {"empty": "", "wrong-value-type": {"synthetic-sensitive-sentinel": True},
                            "invalid-value": "synthetic-sensitive-sentinel", "private-key":
                            source["data"][key] + "-----BEGIN PRIVATE KEY-----\nsynthetic-sensitive-sentinel\n-----END PRIVATE KEY-----"}[case]
                    with self.api(sources) as api:
                        result = self.render(self.values(), kubeconfig=api.kubeconfig)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Headlamp OIDC projection", result.stderr)
                    self.assertNotIn("synthetic-sensitive-sentinel", result.stdout + result.stderr)
                    self.assertNotIn("synthetic-oidc-fixture", result.stdout + result.stderr)
                    self.assertNotIn(self.sources()[0]["data"]["selected-key"], result.stdout + result.stderr)
        with self.api(self.sources(), denied=True) as api:
            self.assertNotEqual(self.render(self.values(), kubeconfig=api.kubeconfig).returncode, 0)
        self.assertIn("requires an existing non-terminating source", self.render(self.values()).stderr)

    def test_rejects_foreign_targets_and_identity_collisions(self):
        with self.api(self.sources()) as api:
            targets = self.targets(documents(self.render(self.values(), kubeconfig=api.kubeconfig)))
        for target in targets:
            for conflict in ("unowned", "release", "namespace", "manager", "terminating"):
                with self.subTest(kind=target["kind"], conflict=conflict):
                    foreign = copy.deepcopy(target)
                    metadata = foreign["metadata"]
                    if conflict == "unowned":
                        metadata.pop("annotations")
                    elif conflict == "release":
                        metadata["annotations"]["meta.helm.sh/release-name"] = "another-release"
                    elif conflict == "namespace":
                        metadata["annotations"]["meta.helm.sh/release-namespace"] = "other"
                    elif conflict == "manager":
                        metadata["labels"]["app.kubernetes.io/managed-by"] = "operator"
                    else:
                        metadata["deletionTimestamp"] = "2026-01-01T00:00:00Z"
                    with self.api(self.sources() + [foreign]) as api:
                        self.assertIn("destination ownership conflict", self.render(self.values(), kubeconfig=api.kubeconfig).stderr)
        # Identical names in different namespaces are distinct, valid objects.
        sources = self.sources()
        sources[0]["metadata"]["name"] = "headlamp-oidc"
        sources[1]["metadata"]["name"] = "headlamp-ca"
        values = self.values()
        values["oidc"]["clientSecret"]["sourceName"] = "headlamp-oidc"
        values["oidc"]["caSourceConfigMap"] = "headlamp-ca"
        with self.api(sources) as api:
            self.assertEqual(len(self.targets(documents(self.render(values, kubeconfig=api.kubeconfig)))), 2)
        for kind in ("Secret", "ConfigMap"):
            values = self.values()
            if kind == "Secret":
                values["oidc"]["clientSecret"]["sourceName"] = "headlamp-oidc"
            else:
                values["oidc"]["clientSecret"].pop("sourceName")
                values["oidc"]["caSourceConfigMap"] = "headlamp-ca"
            # Core's witness requires kars-system, so isolate the maintained Headlamp
            # template to exercise its independent same-namespace identity guard.
            with tempfile.TemporaryDirectory() as directory:
                chart = Path(directory)
                (chart / "templates").mkdir()
                (chart / "Chart.yaml").write_text("apiVersion: v2\nname: kars\nversion: 0.1.0\n")
                (chart / "values.yaml").write_text((CORE / "values.yaml").read_text())
                (chart / "templates/headlamp.yaml").write_text((CORE / "templates/headlamp.yaml").read_text())
                result = render("--namespace", "headlamp", "--set-json", "headlamp=" + json.dumps(values),
                                chart=chart, release="kars")
            self.assertIn("must be distinct objects", result.stderr)
        with self.api(self.sources() + [{"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "headlamp"}}]) as api:
            self.assertIn("outside this Core installation", self.render(self.values(), kubeconfig=api.kubeconfig).stderr)

    def test_rejects_invalid_projection_configuration(self):
        for field in ("sourceName", "name", "key", "caSourceConfigMap", "caConfigMap"):
            for invalid in (True, 42, [], {}, " ", "../escape", "source/key"):
                with self.subTest(field=field, invalid=invalid):
                    values = self.values()
                    container = values["oidc"]["clientSecret"] if field in ("sourceName", "name", "key") else values["oidc"]
                    container[field] = invalid
                    result = self.render(values)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("headlamp.oidc.", result.stderr)
        for field in ("sourceName", "name", "caSourceConfigMap", "caConfigMap"):
            for invalid in ("UpperCase", "a..b", "a" * 254):
                values = self.values()
                container = values["oidc"]["clientSecret"] if field in ("sourceName", "name") else values["oidc"]
                container[field] = invalid
                self.assertIn("must be a Kubernetes object name", self.render(values).stderr)
        for field in ("name", "key", "caConfigMap"):
            values = self.values()
            container = values["oidc"]["clientSecret"] if field != "caConfigMap" else values["oidc"]
            container[field] = ""
            self.assertNotEqual(self.render(values).returncode, 0)
        for values in ({"oidc": "wrong-type"}, {"oidc": {"clientSecret": "wrong-type"}}):
            self.assertIn("must be a map", self.render(values).stderr)
        for disabled in ("headlamp", "oidc"):
            values = self.values()
            (values if disabled == "headlamp" else values["oidc"])["enabled"] = False
            self.assertIn("projection requires", self.render(values).stderr)

    def test_kind_beta_cold_caller_accepts_projected_references(self):
        values = core_values()
        values["headlamp"]["oidc"].update(self.values()["oidc"])
        with self.api(self.sources(), version="1.35.0") as api:
            docs = documents(profile(CORE, values, kubeconfig=api.kubeconfig))
        self.assertEqual(len(self.targets(docs)), 2)
        self.assert_tls(docs, ca=True)


class KindBetaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.core = documents(profile(CORE, core_values()))

    def test_actual_core_caller_wires_ordered_scoped_automatic_operations(self):
        docs = self.core
        self.assertFalse(any(o["kind"] == "Namespace" and o["metadata"]["name"] == "kars-sre" for o in docs))
        self.assertFalse(any((o["kind"], o["metadata"]["name"]) in {
            ("KarsSandbox", "sre"), ("InferencePolicy", "sre-inference"), ("ToolPolicy", "sre-tools")
        } for o in docs))
        intent = json.loads(next(o for o in docs if o["metadata"]["name"] == "kars-sre-first-install")["data"]["intent.json"])
        sandbox = next(o for o in intent["resources"] if o["kind"] == "KarsSandbox")
        self.assertEqual(sandbox["spec"]["runtime"]["kind"], "Hermes")
        self.assertEqual(intent["funding"], core_values()["sre"]["firstInstallFunding"])
        # Dynamic baseline grants belong to sandbox reconciliation, not Helm's
        # first-install SRE hook or a static snapshot of worker namespaces.
        self.assertFalse(any(o["metadata"].get("namespace") == "kars-sre" for o in docs))
        discovery_role = next(o for o in docs if o["kind"] == "Role"
                              and o["metadata"]["name"] == "kars-datapath-witness"
                              and o["metadata"]["namespace"] == "kars-system")
        self.assertIn({"apiGroups": ["kars.azure.com"], "resources": ["karssandboxes"],
                       "verbs": ["get", "list"]}, discovery_role["rules"])
        aggregator = next(o for o in docs if o["kind"] == "Deployment" and o["metadata"]["name"] == "aggregator")
        self.assertIn("--process-health", aggregator["spec"]["template"]["spec"]["containers"][0]["readinessProbe"]["exec"]["command"])
        gadget = next(o for o in docs if o["kind"] == "DaemonSet" and o["metadata"]["name"] == "gadget")
        self.assertEqual(gadget["spec"]["template"]["spec"]["affinity"]["nodeAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"]["nodeSelectorTerms"],
                         [{"matchFields": [{"key": "metadata.name", "operator": "In", "values": ["kind-worker"]}]}])
        config = json.loads(next(o for o in docs if o["metadata"]["name"] == "witness-runtime")["data"]["config.json"])
        self.assertEqual((config["nodes"], config["sandboxes"], config["release_name"], config["scope"]),
                         (["kind-worker"], [], "kars", "all"))

    def test_absent_bootstrap_defaults_preserve_existing_controller_environment(self):
        controller = next(o for o in self.core if o["kind"] == "Deployment" and o["metadata"]["name"] == "kars-controller")
        names = {e["name"] for e in controller["spec"]["template"]["spec"]["containers"][0]["env"]}
        self.assertTrue(names.isdisjoint({"KARS_TASK_DEFAULT_MODEL", "AZURE_OPENAI_DEPLOYMENT", "KARS_TASK_DEFAULT_PROVIDER"}))

    def test_typed_bootstrap_defaults_keep_sre_selection_and_other_environment_unchanged(self):
        values = core_values()
        values["controller"]["inferenceDefaults"] = {"provider": "azure-openai", "model": "task-model"}
        docs = documents(profile(CORE, values))
        def environment(objects):
            controller = next(o for o in objects if o["kind"] == "Deployment" and o["metadata"]["name"] == "kars-controller")
            return {e["name"]: e for e in controller["spec"]["template"]["spec"]["containers"][0]["env"]}
        actual = environment(docs)
        for name, value in [("KARS_TASK_DEFAULT_MODEL", "task-model"), ("AZURE_OPENAI_DEPLOYMENT", "task-model"),
                            ("KARS_TASK_DEFAULT_PROVIDER", "azure-openai")]:
            self.assertEqual(actual.pop(name), {"name": name, "value": value})
        self.assertEqual(actual, environment(self.core))
        intent = json.loads(next(o for o in docs if o["metadata"]["name"] == "kars-sre-first-install")["data"]["intent.json"])
        policy = next(o for o in intent["resources"] if o["kind"] == "InferencePolicy")
        self.assertEqual(policy["spec"]["modelPreference"]["primary"],
                         {"provider": "azure-openai", "deployment": "operator-selected-model"})

    def test_bootstrap_defaults_reject_invalid_values_and_unconfigured_kind_routes(self):
        for defaults in [True, False, ["model"], {"provider": "azure-openai"}, {"model": "task-model"},
                         {"provider": "azure-openai", "model": " "}, {"provider": "azure-openai", "model": "a\nb"},
                         {"provider": "azure-openai", "model": "a" * 257},
                         {"provider": "azure-openai", "model": ["task-model"]},
                         {"provider": "foundry", "model": "task-model"}]:
            with self.subTest(defaults=defaults):
                values = core_values()
                values["controller"]["inferenceDefaults"] = defaults
                self.assertNotEqual(profile(CORE, values).returncode, 0)

    def test_kind_bootstrap_defaults_do_not_allow_raw_environment_overrides(self):
        for name in ["KARS_TASK_DEFAULT_MODEL", "AZURE_OPENAI_DEPLOYMENT", "KARS_TASK_DEFAULT_PROVIDER", "LEADER_ELECTION"]:
            with self.subTest(name=name):
                values = core_values()
                values["controller"]["inferenceDefaults"] = {"provider": "azure-openai", "model": "task-model"}
                values["controller"]["extraEnv"] = [{"name": name, "value": "other"}]
                self.assertNotEqual(profile(CORE, values).returncode, 0)

    def test_generic_bootstrap_defaults_reject_duplicate_environment_bindings(self):
        for name in ["KARS_TASK_DEFAULT_MODEL", "AZURE_OPENAI_DEPLOYMENT", "KARS_TASK_DEFAULT_PROVIDER"]:
            with self.subTest(name=name):
                result = render("--set", "controller.inferenceDefaults.provider=azure-openai,controller.inferenceDefaults.model=task-model",
                                "--set-json", "controller.extraEnv=" + json.dumps([{"name": name, "value": "other"}]),
                                chart=CORE, release="kars")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("must not override controller.inferenceDefaults", result.stderr)

    def test_kind_first_install_funding_rejects_even_large_finite_grants(self):
        for tokens in [1_000_000, 1_500_000, 9_007_199_254_740_991]:
            with self.subTest(tokens=tokens):
                values = core_values()
                values["sre"]["firstInstallFunding"].update(tokens=tokens)
                result = profile(CORE, values)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Unbounded first-install funding requires exactly", result.stderr)

    def test_first_install_funding_rejects_non_integer_or_unsafe_tokens(self):
        for tokens in [0, -1, 1.25, 9_007_199_254_740_992, "1500000", True, None]:
            with self.subTest(tokens=tokens):
                values = core_values()
                values["sre"]["firstInstallFunding"]["tokens"] = tokens
                result = profile(CORE, values)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Unbounded first-install funding requires exactly", result.stderr)

    def test_missing_or_conflicting_operator_inputs_fail_in_actual_helm_caller(self):
        for override in ["schemaHook.enabled=false", "sre.enabled=false", "headlamp.oidc.enabled=false",
                         "datapathWitness.managed=false", "kindBeta.cni=", "cilium.enabled=true",
                         "datapathWitness.nodes=[]", "sandbox.nodeSelector.kubernetes\\.io/hostname=other",
                         "datapathWitness.gadget.hookMode=", "sre.provider=", "sre.model=",
                         "inferenceRouter.azure.openai.credentials.existingSecret=", "inferenceBudget.contracts=[]",
                         "sre.firstInstallFunding.tokens=0", "sre.firstInstallFunding.expiresAt=2000-01-01T00:00:00Z",
                         "controller.image.tag=latest", "headlamp.image.tag=latest", "runtimes.hermes.image=mutable",
                         "headlamp.oidc.issuerURL=http://issuer.example.invalid"]:
            with self.subTest(override=override):
                result = profile(CORE, core_values(), "--set", override)
                self.assertNotEqual(result.returncode, 0, f"accepted unsafe override: {override}")
        self.assertNotEqual(profile(CORE, {}).returncode, 0)

    def test_provider_and_model_selection_reuses_authenticated_foundry_route(self):
        values = core_values()
        values["sre"].update(provider="foundry", model="another-approved-model")
        values["inferenceRouter"]["azure"]["openai"]["endpoint"] = ""
        values["foundry"] = {"endpoint": "https://foundry.example.invalid"}
        values["controller"]["inferenceDefaults"] = {"provider": "foundry", "model": "task-foundry-model"}
        docs = documents(profile(CORE, values))
        controller = next(o for o in docs if o["kind"] == "Deployment" and o["metadata"]["name"] == "kars-controller")
        env = {e["name"]: e.get("value") for e in controller["spec"]["template"]["spec"]["containers"][0]["env"]}
        self.assertEqual(env["KARS_TASK_DEFAULT_PROVIDER"], "foundry")
        self.assertEqual(env["KARS_TASK_DEFAULT_MODEL"], "task-foundry-model")
        intent = json.loads(next(o for o in docs if o["metadata"]["name"] == "kars-sre-first-install")["data"]["intent.json"])
        policy = next(o for o in intent["resources"] if o["kind"] == "InferencePolicy")
        self.assertEqual(policy["spec"]["modelPreference"]["primary"], {"provider": "foundry", "deployment": "another-approved-model"})

    def bridge_api(self, mutate=None, denied=False):
        controller = copy.deepcopy(next(o for o in self.core if o["kind"] == "Deployment" and o["metadata"]["name"] == "kars-controller"))
        controller["metadata"]["uid"] = "core-uid"
        controller["metadata"]["annotations"].update({"meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system"})
        headlamp = copy.deepcopy(next(o for o in self.core if o["kind"] == "Deployment" and o["metadata"]["name"] == "headlamp"))
        registration = {"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSRERegistration",
                        "metadata": {"name": "canonical", "generation": 1}, "spec": {
                            "enabled": True, "controller": {"deployment": {"uid": "core-uid"}},
                            "firstInstallFunding": {"intent": {"uid": "intent-uid"}}},
                        "status": {"phase": "Ready", "observedGeneration": 1}}
        objects = [controller, headlamp, registration]
        if mutate:
            mutate(objects)
        resources = dict(RESOURCES, **{
            "v1": RESOURCES["v1"] + [("services", "Service", True), ("secrets", "Secret", True)],
            "networking.k8s.io/v1": [("networkpolicies", "NetworkPolicy", True)],
            "kars.azure.com/v1alpha1": [("karssreregistrations", "KarsSRERegistration", False)],
        })
        return FakeApi(objects, denied=denied, version="1.35.0", resources=resources)

    def test_bridge_matches_real_core_identity_oidc_and_qualification_contract(self):
        with self.bridge_api() as api:
            docs = documents(profile(BRIDGE, bridge_values(), kubeconfig=api.kubeconfig))
        self.assertFalse(any(o["kind"] == "Namespace" or o["kind"] == "KarsSandbox" for o in docs))
        web = next(o for o in docs if o["kind"] == "Deployment" and o["metadata"]["name"] == "kars-bridge-web")
        env = {o["name"]: o for o in web["spec"]["template"]["spec"]["containers"][0]["env"]}
        self.assertEqual(env["BRIDGE_SESSION_SECRET"]["valueFrom"]["secretKeyRef"]["name"], "bridge-session")
        self.assertEqual(env["BRIDGE_HEADLAMP_URL"]["value"], "http://localhost:3903")

    def ca_config(self):
        # PEM envelope fixture only: render checks are not X.509/TLS qualification.
        return {"apiVersion": "v1", "kind": "ConfigMap",
                "metadata": {"name": "external-oidc-ca", "namespace": "kars-system"},
                "data": {"ca.crt": "-----BEGIN CERTIFICATE-----\nb2ZmbGluZS1maXh0dXJl\n-----END CERTIFICATE-----\n"}}

    def ca_values(self):
        values = bridge_values()
        values["web"]["oidc"] = {"caConfigMap": "external-oidc-ca"}
        return values

    def generic_bridge(self, values):
        args = []
        for key, value in values.items():
            args.extend(["--set-json", key + "=" + json.dumps(value)])
        return render(*args, chart=BRIDGE, release="kars-bridge")

    def assert_ca_mount(self, docs):
        web = next(o for o in docs if o["kind"] == "Deployment" and o["metadata"]["name"] == "kars-bridge-web")
        pod = web["spec"]["template"]["spec"]
        container = pod["containers"][0]
        env = {o["name"]: o for o in container["env"]}
        self.assertEqual(env["NODE_EXTRA_CA_CERTS"]["value"], "/etc/bridge-oidc-ca/ca.crt")
        self.assertEqual(sum(e["name"] == "NODE_EXTRA_CA_CERTS" for e in container["env"]), 1)
        self.assertEqual(next(v for v in pod["volumes"] if v["name"] == "oidc-ca"), {
            "name": "oidc-ca", "configMap": {"name": "external-oidc-ca", "optional": False,
            "defaultMode": 0o444, "items": [{"key": "ca.crt", "path": "ca.crt"}]}})
        self.assertEqual(next(v for v in container["volumeMounts"] if v["name"] == "oidc-ca"), {
            "name": "oidc-ca", "mountPath": "/etc/bridge-oidc-ca", "readOnly": True})
        for key in ["NODE_TLS_REJECT_UNAUTHORIZED", "NODE_OPTIONS", "BRIDGE_OIDC_BACKCHANNEL_ISSUER", "DEX_UPSTREAM_URL"]:
            self.assertNotIn(key, env)
        self.assertFalse(any(o["kind"] == "ConfigMap" and o["metadata"]["name"] == "external-oidc-ca" for o in docs))
        self.assertTrue(container["securityContext"]["readOnlyRootFilesystem"])

    def test_external_oidc_ca_mount_and_kind_beta_lookup(self):
        values = self.ca_values()
        self.assert_ca_mount(documents(self.generic_bridge(values)))
        config = self.ca_config()
        with self.bridge_api(lambda objects: objects.append(config)) as api:
            self.assert_ca_mount(documents(profile(BRIDGE, values, kubeconfig=api.kubeconfig)))
            self.assertIn("/api/v1/namespaces/kars-system/configmaps/external-oidc-ca", api.calls)

    def test_external_oidc_ca_is_default_off_and_leaves_bundled_dex_unchanged(self):
        for values in [{}, {"idp": {"enabled": True}}, bridge_values()]:
            original = documents(self.generic_bridge(values))
            explicit = copy.deepcopy(values)
            explicit.setdefault("web", {})["oidc"] = {"caConfigMap": ""}
            rendered = documents(self.generic_bridge(explicit))
            deployments = lambda docs: [o for o in docs if o["kind"] == "Deployment"]
            self.assertEqual(deployments(original), deployments(rendered))
            web = next(o for o in original if o["kind"] == "Deployment" and o["metadata"]["name"] == "kars-bridge-web")
            pod = web["spec"]["template"]["spec"]
            self.assertNotIn("NODE_EXTRA_CA_CERTS", [e["name"] for e in pod["containers"][0]["env"]])
            self.assertNotIn("oidc-ca", [v["name"] for v in pod["volumes"]])

    def test_external_oidc_ca_rejects_invalid_names_and_types(self):
        for name in [True, 42, [], {}, " ", "../ca", "ca/key", "UpperCase", "-ca", "ca-", "a..b", "a" * 254]:
            with self.subTest(name=name):
                values = self.ca_values()
                values["web"]["oidc"]["caConfigMap"] = name
                result = self.generic_bridge(values)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("web.oidc.caConfigMap", result.stderr)
        values = self.ca_values()
        values["web"]["oidc"] = "not-a-map"
        self.assertIn("web.oidc must be a map", self.generic_bridge(values).stderr)

    def test_external_oidc_ca_rejects_missing_duplicate_or_unsafe_issuer(self):
        for issuer in ["", "http://issuer.example.invalid", "https://user:pass@issuer.example.invalid",
                       "https://issuer.example.invalid?query", "https://issuer.example.invalid/#fragment",
                       "https://issuer.example.invalid:0", "https://issuer.example.invalid:65536"]:
            with self.subTest(issuer=issuer):
                values = self.ca_values()
                values["web"]["extraEnv"][0]["value"] = issuer
                self.assertNotEqual(self.generic_bridge(values).returncode, 0)
        for replacement in [[], [{"name": "BRIDGE_OIDC_ISSUER", "valueFrom": {
                "secretKeyRef": {"name": "issuer", "key": "url"}}}], bridge_values()["web"]["extraEnv"][:1] * 2]:
            values = self.ca_values()
            values["web"]["extraEnv"] = replacement
            self.assertNotEqual(self.generic_bridge(values).returncode, 0)
        values = self.ca_values()
        values["idp"] = {"enabled": True}
        self.assertIn("only supported for external OIDC", self.generic_bridge(values).stderr)

    def test_external_oidc_ca_rejects_env_trust_and_backchannel_overrides(self):
        for key in ["NODE_EXTRA_CA_CERTS", "NODE_TLS_REJECT_UNAUTHORIZED", "NODE_OPTIONS",
                    "BRIDGE_OIDC_BACKCHANNEL_ISSUER", "DEX_UPSTREAM_URL"]:
            with self.subTest(key=key):
                values = self.ca_values()
                values["web"]["extraEnv"].append({"name": key, "value": "unsafe"})
                self.assertIn("conflicts with web.extraEnv", self.generic_bridge(values).stderr)
                with self.bridge_api(lambda objects: objects.append(self.ca_config())) as api:
                    self.assertNotEqual(profile(BRIDGE, values, kubeconfig=api.kubeconfig).returncode, 0)

    def test_kind_beta_ca_requires_existing_public_pem_in_exact_namespace(self):
        configs = [None, self.ca_config(), self.ca_config(), self.ca_config(), self.ca_config(),
                   self.ca_config(), self.ca_config()]
        configs[1]["metadata"]["namespace"] = "other"
        configs[2]["metadata"]["deletionTimestamp"] = "2026-01-01T00:00:00Z"
        configs[3]["data"] = {}
        configs[4]["data"]["ca.crt"] = ""
        configs[5]["data"]["ca.crt"] = "not PEM"
        configs[6]["data"]["ca.crt"] += "-----BEGIN PRIVATE KEY-----\nprivate\n-----END PRIVATE KEY-----"
        for config in configs:
            with self.subTest(config=config):
                with self.bridge_api(lambda objects: objects.extend([config] if config else [])) as api:
                    result = profile(BRIDGE, self.ca_values(), kubeconfig=api.kubeconfig)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("external OIDC CA ConfigMap", result.stderr)
        with self.bridge_api(lambda objects: objects.append(self.ca_config()), denied=True) as api:
            self.assertNotEqual(profile(BRIDGE, self.ca_values(), kubeconfig=api.kubeconfig).returncode, 0)

    def test_kind_beta_ca_does_not_relax_https_port_or_environment_allowlist(self):
        for enabled in [False, True]:
            values = self.ca_values() if enabled else bridge_values()
            with self.bridge_api(lambda objects: objects.append(self.ca_config())) as api:
                values["web"]["extraEnv"][0]["value"] = "https://issuer.example.invalid:5557"
                self.assertIn("HTTPS OIDC on port 443", profile(BRIDGE, values, kubeconfig=api.kubeconfig).stderr)
                values["web"]["extraEnv"][0]["value"] = "https://issuer.example.invalid"
                values["web"]["extraEnv"].append({"name": "BRIDGE_DEV_ROLE", "value": "admin"})
                self.assertIn("permits only unique genuine OIDC settings", profile(BRIDGE, values, kubeconfig=api.kubeconfig).stderr)

    def test_bridge_rejects_missing_foreign_partial_or_denied_core(self):
        self.assertNotEqual(profile(BRIDGE, bridge_values()).returncode, 0)
        mutations = [lambda o: o[0]["metadata"]["annotations"].update({"meta.helm.sh/release-name": "foreign"}),
                     lambda o: o[0]["metadata"].update(uid="replacement"),
                     lambda o: o[1]["metadata"]["annotations"].clear(),
                     lambda o: o[2]["status"].update(phase="Pending"),
                     lambda o: o[2]["status"].update(observedGeneration=0),
                     lambda o: o[2]["spec"].update(enabled=False)]
        for mutate in mutations:
            with self.bridge_api(mutate) as api:
                self.assertNotEqual(profile(BRIDGE, bridge_values(), kubeconfig=api.kubeconfig).returncode, 0)
        with self.bridge_api(denied=True) as api:
            self.assertNotEqual(profile(BRIDGE, bridge_values(), kubeconfig=api.kubeconfig).returncode, 0)

    def test_bridge_rejects_identity_mismatch_fallback_and_missing_inputs(self):
        with self.bridge_api() as api:
            for override in ["web.image.tag=latest", "auth.principalSecretName=", "bff.routeQualification.mode=validation",
                             "headlamp.url=http://localhost:9999", "web.extraEnv[0].value=https://other.example.invalid",
                             "web.extraEnv[1].name=BRIDGE_DEV_ROLE", "web.extraEnv[5].valueFrom.secretKeyRef.optional=true",
                             "networkPolicy.create=false", "networkPolicy.routerApiServerIngress=true"]:
                with self.subTest(override=override):
                    self.assertNotEqual(profile(BRIDGE, bridge_values(), "--set", override, kubeconfig=api.kubeconfig).returncode, 0)
            self.assertNotEqual(profile(BRIDGE, {}, kubeconfig=api.kubeconfig).returncode, 0)


if __name__ == "__main__":
    unittest.main()
