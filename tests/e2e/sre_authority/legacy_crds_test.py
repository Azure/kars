# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pure historical fixture checks, not a substitute for hosted Kind acceptance."""

import copy
from pathlib import Path
import types
import unittest
from unittest.mock import Mock, patch

from sre_authority.legacy_crds import (
    CRDS, GROUP_VERSION, IDENTITIES, bootstrap_legacy_crds, render_legacy_crds, validate_rendered_crds,
)
from sre_authority.registration_schema import CRD_NAME


def historical_objects():
    def obj(plural, kind, scope):
        return {
            "apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": {"name": f"{plural}.kars.azure.com", "labels": {"app.kubernetes.io/name": "kars"}},
            "spec": {"group": "kars.azure.com", "scope": scope, "names": {"plural": plural, "kind": kind},
                     "versions": [{"name": "v1alpha1", "served": True, "storage": True,
                                   "schema": {"openAPIV3Schema": {"type": "object"}}}]},
        }
    result = {filename: [obj(*identity)] for filename, identity in CRDS.items()}
    result["crd.yaml"].append(obj(*IDENTITIES[-1]))
    return result


def flattened(historical):
    return [obj for objects in historical.values() for obj in objects]


def discovery():
    return {"kind": "APIResourceList", "groupVersion": GROUP_VERSION, "resources": [
        {"name": plural, "kind": kind, "namespaced": scope == "Namespaced",
         "verbs": ["create", "get", "list", "watch"]}
        for plural, kind, scope in IDENTITIES
    ]}


class FixtureHarness:
    def __init__(self):
        self.events = []
        self.responses = [(200, discovery())]
        self.existing = None
        self.create_failure = False
        self.wait_failure = False

    def api(self, method, path, *, status):
        self.events.append(("api", method, path))
        if path == f"/apis/{GROUP_VERSION}":
            code, body = self.responses.pop(0)
        elif path.endswith("/" + str(self.existing)):
            code, body = 200, {"kind": "CustomResourceDefinition"}
        else:
            code, body = 404, {"kind": "Status", "reason": "NotFound"}
        if code not in (status if isinstance(status, tuple) else (status,)):
            raise AssertionError(f"HTTP {code}")
        return types.SimpleNamespace(status_code=code, json=lambda: body)

    def create(self, obj):
        self.events.append(("create", obj))
        if self.create_failure:
            raise AssertionError("HTTP 409")

    def k(self, *args, **kwargs):
        self.events.append(("wait", args, kwargs))
        if self.wait_failure:
            raise AssertionError("wait-timeout")

    def poll(self, label, predicate, *, seconds):
        self.events.append(("poll", label, seconds))
        for _ in range(3):
            if predicate():
                return True
        raise AssertionError("bounded discovery deadline")

    def passed(self, message):
        self.events.append(("passed", message))


class LegacyCRDTests(unittest.TestCase):
    def bootstrap(self, h):
        objects = flattened(historical_objects())
        with patch("sre_authority.legacy_crds.render_legacy_crds", return_value=objects):
            bootstrap_legacy_crds(h, Path("unused-test-chart"), {})
        return objects

    def test_render_requires_exact_historical_content_not_just_a_matching_name(self):
        historical = historical_objects()
        rendered = flattened(copy.deepcopy(historical))
        self.assertEqual(validate_rendered_crds(rendered, historical), rendered)
        rendered[0]["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["description"] = "changed"
        with self.assertRaises(AssertionError):
            validate_rendered_crds(rendered, historical)

    def test_current_registration_extra_missing_duplicate_and_foreign_resources_are_rejected(self):
        historical = historical_objects()
        valid = flattened(copy.deepcopy(historical))
        registration = copy.deepcopy(valid[0])
        registration["metadata"]["name"] = CRD_NAME
        for rendered in (valid[:-1], valid + [registration], [registration] + valid[1:],
                         [valid[0]] + valid[:-1], [{"kind": "Secret"}] + valid[1:]):
            with self.subTest(count=len(rendered)), self.assertRaises(AssertionError):
                validate_rendered_crds(rendered, historical)
        with self.assertRaises(AssertionError):
            validate_rendered_crds(valid, {**historical, "crd-karssreregistration.yaml": registration})

    def test_historical_identity_schema_and_preexisting_ownership_are_strict(self):
        def changed(section, key, value):
            obj = historical_objects()
            obj["crd.yaml"][0][section][key] = value
            return obj
        invalid = [
            changed("metadata", "name", "other.kars.azure.com"),
            changed("metadata", "annotations", {"meta.helm.sh/release-name": "other"}),
            changed("metadata", "ownerReferences", [{"uid": "foreign"}]),
            changed("spec", "group", "foreign.example"),
            changed("spec", "scope", "Cluster"),
            changed("spec", "names", {"plural": "karssandboxes", "kind": "Secret"}),
            changed("spec", "versions", []),
        ]
        for historical in invalid:
            with self.subTest(historical=historical["crd.yaml"]), self.assertRaises(AssertionError):
                validate_rendered_crds(flattened(historical), historical)

    def test_changed_extracted_source_and_unexpected_inventory_fail_before_render(self):
        h = Mock()
        # A small path double avoids filesystem writes in these pure checks.
        class Chart:
            def __truediv__(self, _name):
                return self

            def glob(self, _pattern):
                return [types.SimpleNamespace(name=filename) for filename in CRDS]

            def read_text(self):
                return "changed"
        for sources in ({filename: "original" for filename in CRDS},
                        {filename: "{{ templated }}" for filename in CRDS},
                        {"crd-karssreregistration.yaml": "unexpected"}):
            with self.assertRaises(AssertionError):
                render_legacy_crds(h, Chart(), sources)
        h.run.assert_not_called()

    def test_create_only_ownership_and_all_readiness_precede_hooks(self):
        h = FixtureHarness()
        originals = self.bootstrap(h)
        names = [obj["metadata"]["name"] for obj in originals]
        self.assertEqual(len(names), 18)
        self.assertNotIn(CRD_NAME, names)
        self.assertEqual([event[2].rsplit("/", 1)[-1] for event in h.events[:19]],
                         [CRD_NAME] + names)
        creates = [event[1] for event in h.events if event[0] == "create"]
        self.assertEqual(len(creates), 18)
        for original, created in zip(originals, creates):
            self.assertNotIn("annotations", original["metadata"])
            self.assertEqual(created["spec"], original["spec"])
            self.assertEqual(created["metadata"]["labels"]["app.kubernetes.io/managed-by"], "Helm")
            self.assertEqual(created["metadata"]["annotations"], {
                "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system"})
        wait = next(event for event in h.events if event[0] == "wait")
        self.assertEqual(wait[1], ("wait", "--for=condition=Established",
                                  *[f"crd/{name}" for name in names], "--timeout=60s"))
        self.assertLess(h.events.index(wait), next(i for i, e in enumerate(h.events) if e[0] == "poll"))
        self.assertEqual(h.events[-1][0], "passed")
        fixture = (Path(__file__).with_name("fixtures.py")).read_text()
        self.assertLess(fixture.index("bootstrap_legacy_crds(h,"), fixture.index('h.run(["helm", "install"'))
        self.assertLess(fixture.index('h.run(["helm", "install"'), fixture.index('h.get("toolpolicy"'))
        for bypass in ("--take-ownership", "--force", "--validate=false", "--no-hooks", "governance.enabled=false"):
            self.assertNotIn(bypass, fixture)

    def test_existing_foreign_or_even_release_named_crd_is_never_adopted(self):
        for name in (CRD_NAME, "toolpolicies.kars.azure.com", "karssandboxes.kars.azure.com"):
            h = FixtureHarness()
            h.existing = name
            with self.subTest(name=name), self.assertRaisesRegex(AssertionError, "HTTP 200"):
                self.bootstrap(h)
            self.assertFalse(any(event[0] in ("create", "wait", "passed") for event in h.events))

    def test_create_conflict_is_not_retried_as_apply_or_patch(self):
        h = FixtureHarness()
        h.create_failure = True
        with self.assertRaisesRegex(AssertionError, "HTTP 409"):
            self.bootstrap(h)
        self.assertEqual(sum(event[0] == "create" for event in h.events), 1)
        self.assertFalse(any(event[0] in ("wait", "passed") for event in h.events))

    def test_established_failure_never_reaches_discovery_or_hooks(self):
        h = FixtureHarness()
        h.wait_failure = True
        with self.assertRaisesRegex(AssertionError, "wait-timeout"):
            self.bootstrap(h)
        self.assertFalse(any(event[0] in ("poll", "passed") for event in h.events))

    def test_discovery_retries_only_missing_group_or_resources(self):
        h = FixtureHarness()
        incomplete = discovery()
        incomplete["resources"].pop()
        h.responses = [(404, {"kind": "Status", "reason": "NotFound"}),
                       (200, incomplete), (200, discovery())]
        self.bootstrap(h)
        self.assertEqual(h.responses, [])
        self.assertEqual(h.events[-1][0], "passed")

    def test_discovery_auth_server_errors_and_malformed_responses_are_fatal(self):
        for response in ((403, {"kind": "Status", "reason": "Forbidden"}),
                         (401, {"kind": "Status", "reason": "Unauthorized"}),
                         (500, {"kind": "Status", "reason": "InternalError"}),
                         (404, {"kind": "Status", "reason": "Forbidden"}),
                         (200, {"kind": "Secret", "data": {"token": "must-not-log"}})):
            h = FixtureHarness()
            h.responses = [response, (200, discovery())]
            with self.subTest(code=response[0]), self.assertRaises(AssertionError) as failure:
                self.bootstrap(h)
            self.assertNotIn("must-not-log", str(failure.exception))
            self.assertEqual(len(h.responses), 1)
            self.assertFalse(any(event[0] == "passed" for event in h.events))

    def test_wrong_discovered_identity_or_verbs_fail_instead_of_retrying(self):
        for key, value in (("kind", "Secret"), ("namespaced", False), ("verbs", ["get"])):
            h = FixtureHarness()
            body = discovery()
            body["resources"][0][key] = value
            h.responses = [(200, body)]
            with self.subTest(key=key), self.assertRaises(AssertionError):
                self.bootstrap(h)
            self.assertFalse(any(event[0] == "passed" for event in h.events))

    def test_discovery_deadline_is_not_success(self):
        h = FixtureHarness()
        body = discovery()
        body["resources"] = []
        h.responses = [(200, body)] * 3
        with self.assertRaisesRegex(AssertionError, "bounded discovery deadline"):
            self.bootstrap(h)
        self.assertFalse(any(event[0] == "passed" for event in h.events))


if __name__ == "__main__":
    unittest.main()
