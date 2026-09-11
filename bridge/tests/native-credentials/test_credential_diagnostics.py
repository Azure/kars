import copy
import json
import types
import unittest

from credential_diagnostics import CATEGORIES, PREFIX, condition_category, runtime_scope
from native_api import Failure

PRIVATE = "DO-NOT-EMIT-PRIVATE-VALUES-OR-API-BODIES"


class CredentialDiagnosticsTests(unittest.TestCase):
    def test_only_exact_known_messages_become_fixed_categories(self):
        for message, category in CATEGORIES.items():
            self.assertEqual(condition_category(message), category)
            self.assertEqual(condition_category("CredentialSourceUnavailable: " + message), category)
            self.assertEqual(condition_category(message + PRIVATE), "unclassified")
        for message in (None, {}, PRIVATE, "prefix" + next(iter(CATEGORIES))):
            self.assertEqual(condition_category(message), "unclassified")

    def test_governed_failure_details_only_retain_known_categories_and_http_codes(self):
        prefix = "CredentialSourceUnavailable: governed credential source or operator grant is unavailable "
        self.assertEqual(condition_category(prefix + "[target_api; code=Some(404)]"),
                         "source_or_grant:target_api:404")
        self.assertEqual(condition_category(prefix + "[bundle_owner; code=None]"),
                         "source_or_grant:bundle_owner")
        for suffix in ("[target_api; code=Some(999)]", "[private_value; code=None]",
                       "[target_api; code=Some(404)]" + PRIVATE, PRIVATE):
            self.assertEqual(condition_category(prefix + suffix), "unclassified")

    def test_scope_capture_reports_only_booleans_and_fixed_states(self):
        sandbox = {"metadata": {"name": "test", "namespace": "work", "uid": "sandbox",
                               "annotations": {"kars.azure.com/namespace-uid": "runtime"}}}
        namespace = {"metadata": {"uid": "runtime", "annotations": {
            PREFIX + "state": "Qualified", PREFIX + "epoch": PRIVATE,
            "kars.azure.com/sandbox-namespace": "work", "kars.azure.com/sandbox-uid": "sandbox",
            "unrelated-private": PRIVATE,
        }}}
        grant = {"metadata": {"generation": 2}, "spec": {"privateActivation": {"namespaces": [
            {"namespace": {"name": "kars-test", "uid": "runtime"}, "epoch": PRIVATE},
        ]}}, "status": {"observedGeneration": 2, "conditions": [
            {"type": "WriterReady", "status": "True", "message": PRIVATE},
            {"type": "PrivateConsumptionReady", "status": "True"},
        ]}}
        responses = {"/api/v1/namespaces/kars-test": namespace,
                     "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace": grant}
        setup = types.SimpleNamespace(admin=types.SimpleNamespace(optional=responses.__getitem__))
        result = runtime_scope(setup, sandbox)
        for key in ("available", "namespacePresent", "grantPresent", "namespaceUidMatches",
                    "namespaceOwnerMatches", "grantScopeIncluded", "epochMatches",
                    "writerReady", "privateConsumptionReady"):
            self.assertIs(result[key], True, key)
        self.assertNotIn(PRIVATE, json.dumps(result))
        stale = copy.deepcopy(grant)
        stale["status"]["observedGeneration"] = 1
        responses[next(path for path in responses if path.startswith("/apis/"))] = stale
        result = runtime_scope(setup, sandbox)
        self.assertFalse(result["writerReady"])
        self.assertFalse(result["privateConsumptionReady"])
        stale["spec"]["privateActivation"]["namespaces"].clear()
        result = runtime_scope(setup, sandbox)
        self.assertFalse(result["grantScopeIncluded"])
        self.assertFalse(result["epochMatches"])
        self.assertNotIn(PRIVATE, json.dumps(result))

    def test_missing_or_failed_metadata_never_claims_qualified_scope(self):
        sandbox = {"metadata": {"name": "test", "namespace": "work"}}
        setup = types.SimpleNamespace(admin=types.SimpleNamespace(optional=lambda _path: None))
        self.assertEqual(runtime_scope(setup, sandbox), {
            "available": True, "namespacePresent": False, "grantPresent": False,
        })
        def failed(_path):
            raise Failure(PRIVATE)
        setup.admin.optional = failed
        result = runtime_scope(setup, sandbox)
        self.assertEqual(result, {"available": False, "category": "api-unavailable"})
        self.assertNotIn(PRIVATE, json.dumps(result))


if __name__ == "__main__":
    unittest.main()
