# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pure historical fixture checks, not a substitute for hosted Kind acceptance."""

import copy
from pathlib import Path
import types
import unittest
from unittest.mock import Mock, patch

from sre_authority.legacy_crds import (
    CRDS, IDENTITIES, preflight_legacy_crds, render_legacy_crds, validate_rendered_crds,
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


class LegacyCRDTests(unittest.TestCase):
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
        h.k.assert_not_called()

    def test_every_absence_is_read_only_before_native_helm_creates_any_crd(self):
        h = Mock()
        h.api.return_value = types.SimpleNamespace(json=lambda: {"kind": "Status", "reason": "NotFound"})
        objects = flattened(historical_objects())
        with patch("sre_authority.legacy_crds.render_legacy_crds", return_value=objects):
            preflight_legacy_crds(h, Path("unused-test-chart"), {})
        names = [CRD_NAME] + [obj["metadata"]["name"] for obj in objects]
        self.assertEqual(len(names), 19)
        self.assertEqual([call.args[1].rsplit("/", 1)[-1] for call in h.api.call_args_list], names)
        self.assertTrue(all(call.args[0] == "GET" and call.kwargs == {"status": 404}
                            for call in h.api.call_args_list))
        h.create.assert_not_called()
        h.k.assert_not_called()
        h.passed.assert_called_once()

    def test_existing_or_inaccessible_crd_aborts_without_adoption_or_retry(self):
        for code in (200, 401, 403, 409, 500):
            h = Mock()
            h.api.side_effect = AssertionError(f"HTTP {code}")
            with patch("sre_authority.legacy_crds.render_legacy_crds",
                       return_value=flattened(historical_objects())):
                with self.subTest(code=code), self.assertRaisesRegex(AssertionError, f"HTTP {code}"):
                    preflight_legacy_crds(h, Path("unused-test-chart"), {})
            h.api.assert_called_once()
            h.create.assert_not_called()
            h.passed.assert_not_called()

    def test_false_not_found_is_not_absence_proof(self):
        for body in ({"kind": "Secret", "reason": "NotFound"}, {"kind": "Status", "reason": "Forbidden"}):
            h = Mock()
            h.api.return_value = types.SimpleNamespace(json=lambda: body)
            with patch("sre_authority.legacy_crds.render_legacy_crds",
                       return_value=flattened(historical_objects())), self.assertRaises(AssertionError):
                preflight_legacy_crds(h, Path("unused-test-chart"), {})
            h.create.assert_not_called()
            h.passed.assert_not_called()

    def test_initial_helm_uses_existing_versioned_waiter_without_custom_creation(self):
        fixture = Path(__file__).with_name("fixtures.py").read_text()
        install = fixture.split("def install_historical_chart(h):", 1)[1].split("def prepare_legacy(h):", 1)[0]
        self.assertLess(install.index("preflight_legacy_crds(h,"), install.index('h.run(["helm", "install"'))
        self.assertIn('sre_migration_helm_wait_arg "$2"', install)
        self.assertIn('wait_arg, "--timeout", "120s"', install)
        self.assertLess(install.index('h.run(["helm", "install"'), install.index('h.get("toolpolicy"'))
        self.assertNotIn("h.create(", install)
        for bypass in ("--take-ownership", "--force", "--validate=false", "--no-hooks", "governance.enabled=false"):
            self.assertNotIn(bypass, fixture)


if __name__ == "__main__":
    unittest.main()
