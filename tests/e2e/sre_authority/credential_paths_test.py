# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Fixture contract checks, not substitutes for hosted Kubernetes acceptance."""

import base64
import copy
import io
import unittest
from contextlib import redirect_stdout
from unittest.mock import patch

from sre_authority.common import OWNER, RUNTIME, require
from sre_authority.credential_paths import (
    SA_NAME, SA_UID, SYNTHETIC_TOKEN, TOKEN_ALIAS, TOKEN_TYPE,
    assert_fresh_private_identity, assert_secret_unchanged, assert_unissued,
    delete_owned, owned_fixtures, seed_privacy_gaps, synthetic_token_secret, token_secret_denials,
)
from sre_authority.harness_test import Response

POLICY = "kars-sre-no-legacy-tokens"
CA = "-----BEGIN CERTIFICATE-----\npublic-test-ca\n-----END CERTIFICATE-----\n"
SECRET_PATH = f"/api/v1/namespaces/{RUNTIME}/secrets"


class FixtureHarness:
    def __init__(self):
        self.state = {}
        self.phase = "legacy"
        self.objects = {}
        self.requests = []
        self.identities = []
        self.sequence = 0

    def create(self, obj):
        obj = copy.deepcopy(obj)
        self.sequence += 1
        obj["metadata"].update(uid=f"uid-{self.sequence}", resourceVersion=str(self.sequence))
        self.objects[obj["kind"].lower(), obj["metadata"]["name"]] = obj
        return copy.deepcopy(obj)

    def get(self, kind, name, _namespace=None):
        if kind == "configmap" and name == "kube-root-ca.crt":
            return {"data": {"ca.crt": CA}}
        if kind == "karssreregistrations.kars.azure.com":
            kind = "karssreregistration"
        return copy.deepcopy(self.objects.get((kind, name)))

    def poll(self, _label, predicate, **_kwargs):
        result = predicate()
        require(result, "Unit fixture did not satisfy the real harness predicate")
        return result

    def passed(self, _message):
        pass

    def token_identity(self, *args):
        self.identities.append(args)

    def api(self, method, path, *, body=None, user="admin", status=None):
        self.requests.append((method, path, copy.deepcopy(body), user))
        resource = path.split("/")[-2]
        kind = {"secrets": "secret", "serviceaccounts": "serviceaccount"}.get(resource)
        key = (kind, path.split("/")[-1])
        current = self.objects.get(key)
        if method == "POST":
            private = (body.get("type") == TOKEN_TYPE
                       and body["metadata"].get("annotations", {}).get(SA_NAME) == "sre-api-router")
            response = Response(403, {"kind": "Status", "reason": "Forbidden", "message": POLICY}) if private \
                else Response(201, self.create(body))
        elif method == "PATCH":
            if current is None:
                response = Response(404, {"kind": "Status", "reason": "NotFound"})
            elif "type" in body:
                response = Response(422, {"kind": "Status", "reason": "Invalid", "details": {"causes": [
                    {"field": "type", "reason": "FieldValueInvalid", "message": "field is immutable"}]}})
            else:
                response = Response(403, {"kind": "Status", "reason": "Forbidden", "message": POLICY})
        elif method == "DELETE":
            if current is None:
                response = Response(404, {"kind": "Status", "reason": "NotFound"})
            elif body["preconditions"] != {k: current["metadata"][k] for k in ("uid", "resourceVersion")}:
                response = Response(409, {"kind": "Status", "reason": "Conflict"})
            else:
                del self.objects[key]
                response = Response(200, {})
        else:
            raise AssertionError("Unexpected unit fixture request")
        if status is not None:
            require(response.status_code in (status if isinstance(status, tuple) else (status,)),
                    f"Unit API HTTP {response.status_code}")
        return response


class CredentialFixtureTests(unittest.TestCase):
    def seeded(self):
        h = FixtureHarness()
        seed_privacy_gaps(h)
        return h

    def test_seed_uses_live_unowned_account_and_complete_noncredential_data(self):
        h = self.seeded()
        account = h.state["prestaged_account"]
        secret = h.state["prestaged_alias"]
        self.assertFalse(account["automountServiceAccountToken"])
        for field in ("ownerReferences", "annotations", "labels", "finalizers"):
            self.assertNotIn(field, account["metadata"])
        self.assertNotIn("secrets", account)
        self.assertEqual(secret["metadata"]["annotations"], {
            SA_NAME: "sre-api-router", SA_UID: account["metadata"]["uid"]})
        self.assertEqual({key: base64.b64decode(value).decode() for key, value in secret["data"].items()},
                         {"token": SYNTHETIC_TOKEN, "ca.crt": CA, "namespace": RUNTIME})
        self.assertNotIn(".", SYNTHETIC_TOKEN)
        self.assertNotIn("finalizers", secret["metadata"])
        self.assertEqual(h.identities, [("watch-only", RUNTIME, "e2e-watch-only")])

    def test_synthetic_secret_requires_current_account_uid_namespace_and_ca(self):
        h = self.seeded()
        account = h.state["prestaged_account"]
        for changed in ({"uid": ""}, {"namespace": "other"}, {"deletionTimestamp": "now"}):
            bad = copy.deepcopy(account)
            bad["metadata"].update(changed)
            with self.subTest(changed=changed), self.assertRaises(AssertionError):
                synthetic_token_secret(h, "probe", bad)
        with patch.object(h, "get", return_value={"data": {}}), self.assertRaises(AssertionError):
            synthetic_token_secret(h, "probe", account)

    def test_missing_replaced_modified_or_populated_fixture_never_proves_denial(self):
        secret = self.seeded().state["prestaged_alias"]
        assert_secret_unchanged(copy.deepcopy(secret), secret)
        variants = [None]
        for key in ("uid", "resourceVersion", "annotations", "finalizers", "ownerReferences"):
            bad = copy.deepcopy(secret)
            bad["metadata"][key] = "changed"
            variants.append(bad)
        for changed in ({"data": {}}, {"data": {"token": "unexpected"}}, {"type": "Opaque"}):
            variants.append({**secret, **changed})
        for bad in variants:
            with self.subTest(case=variants.index(bad)), self.assertRaises(AssertionError):
                assert_secret_unchanged(bad, secret)

    def test_real_probe_contract_preserves_old_object_and_uses_live_benign_sa(self):
        for before in (True, False):
            with self.subTest(before=before):
                h = self.seeded()
                token_secret_denials(h, before_enrollment=before)
                posts = [body for method, path, body, _ in h.requests
                         if method == "POST" and path == SECRET_PATH]
                tokens = [obj for obj in posts if obj["type"] == TOKEN_TYPE]
                self.assertEqual(len(tokens), 3)  # Two CREATE denials and the annotation fixture.
                for obj in tokens:
                    self.assertTrue(obj["metadata"]["annotations"][SA_UID])
                    self.assertEqual(base64.b64decode(obj["data"]["token"]).decode(), SYNTHETIC_TOKEN)
                annotation = next(obj for obj in tokens if obj["metadata"]["name"].endswith("-annotation"))
                self.assertNotEqual(annotation["metadata"]["annotations"][SA_NAME], "sre-api-router")
                escapes = [body for method, path, body, _ in h.requests
                           if method == "PATCH" and path.endswith("/" + TOKEN_ALIAS)]
                self.assertEqual(len(escapes), 2 if before else 0)
                for body in escapes:
                    self.assertEqual(body["metadata"]["uid"], h.state["prestaged_alias"]["metadata"]["uid"])
                    self.assertEqual(body["metadata"]["resourceVersion"],
                                     h.state["prestaged_alias"]["metadata"]["resourceVersion"])
                self.assertEqual(h.get("secret", TOKEN_ALIAS), h.state["prestaged_alias"])
                self.assertFalse(any(name.startswith("e2e-token-") for _, name in h.objects))

    def test_cleanup_accepts_only_real_absence_and_never_drops_cas_fences(self):
        h = self.seeded()
        obj = h.state["prestaged_alias"]
        path = SECRET_PATH + "/" + TOKEN_ALIAS
        delete_owned(h, path, obj)
        delete_owned(h, path, obj)  # Already absent is cleanup, never admission proof.
        self.assertEqual(h.requests[-1][2]["preconditions"], {
            key: obj["metadata"][key] for key in ("uid", "resourceVersion")})
        replacement = h.create({**obj, "metadata": {"name": TOKEN_ALIAS, "namespace": RUNTIME}})
        with self.assertRaisesRegex(AssertionError, "409"):
            delete_owned(h, path, obj)
        self.assertEqual(h.get("secret", TOKEN_ALIAS), replacement)
        with patch.object(h, "api", return_value=Response(404, {})), self.assertRaises(AssertionError):
            delete_owned(h, path, obj)

    def test_cleanup_conflict_preserves_primary_error_and_attempts_independent_fixtures(self):
        h = self.seeded()
        output = io.StringIO()
        with patch("sre_authority.credential_paths.delete_owned", side_effect=AssertionError("private-body")) as cleanup:
            with redirect_stdout(output), self.assertRaisesRegex(AssertionError, "^primary denial failure$"):
                with owned_fixtures(h) as fixtures:
                    fixtures.extend([("first", {}), ("second", {})])
                    raise AssertionError("primary denial failure")
            self.assertEqual([call.args[1] for call in cleanup.call_args_list], ["second", "first"])
            self.assertNotIn("private-body", output.getvalue())
            with self.assertRaisesRegex(AssertionError, "Fixture cleanup failed"):
                with owned_fixtures(h) as fixtures:
                    fixtures.append(("first", {}))

    def test_secret_conflict_preserves_account_to_prevent_gc_of_foreign_replacement(self):
        h = self.seeded()
        original = h.state["prestaged_alias"]
        replacement = h.create(original)
        account = h.state["prestaged_account"]
        with self.assertRaisesRegex(AssertionError, "Fixture cleanup failed"):
            with owned_fixtures(h) as fixtures:
                fixtures.append((f"/api/v1/namespaces/{RUNTIME}/serviceaccounts/sre-api-router", account))
                fixtures.append((SECRET_PATH + "/" + TOKEN_ALIAS, original))
        self.assertEqual(h.get("secret", TOKEN_ALIAS), replacement)
        self.assertEqual(h.get("serviceaccount", "sre-api-router"), account)
        self.assertEqual(len(h.requests), 1)

    def test_disappeared_fixture_patch_404_is_fatal_even_when_cleanup_is_404(self):
        h = self.seeded()
        original = h.api
        def missing(method, path, **kwargs):
            if method == "PATCH":
                h.objects.pop(("secret", path.split("/")[-1]), None)
                return Response(404, {"kind": "Status", "reason": "NotFound"})
            return original(method, path, **kwargs)
        with patch.object(h, "api", side_effect=missing), self.assertRaisesRegex(AssertionError, "got HTTP 404"):
            token_secret_denials(h, before_enrollment=True)

    def test_blocked_controller_cannot_adopt_or_record_same_name_fixture(self):
        h = self.seeded()
        h.create({"kind": "Deployment", "metadata": {"name": "sre"}, "spec": {"replicas": 1}})
        reg = h.create({"kind": "KarsSRERegistration", "metadata": {"name": "canonical"}, "status": {}})
        assert_unissued(h, {"legacyBindings": []}, {})
        h.objects["karssreregistration", "canonical"]["status"]["routerServiceAccountUid"] = \
            h.state["prestaged_account"]["metadata"]["uid"]
        with self.assertRaises(AssertionError):
            assert_unissued(h, {"legacyBindings": []}, {})
        h.objects["karssreregistration", "canonical"] = reg
        h.objects["serviceaccount", "sre-api-router"]["metadata"]["annotations"] = {OWNER: reg["metadata"]["uid"]}
        with self.assertRaises(AssertionError):
            assert_unissued(h, {"legacyBindings": []}, {})

    def test_trusted_private_identity_requires_cleanup_new_uid_and_registration_owner(self):
        h = self.seeded()
        reg = {"metadata": {"uid": "registration"}, "status": {}}
        with self.assertRaises(AssertionError):
            assert_fresh_private_identity(h, reg)
        h.state["prestaged_account_cleaned"] = True
        del h.objects["secret", TOKEN_ALIAS]
        original = h.state["prestaged_account"]
        fresh = h.create({"kind": "ServiceAccount", "metadata": {
            "name": "sre-api-router", "namespace": RUNTIME, "annotations": {OWNER: "registration"}},
            "automountServiceAccountToken": False})
        reg["status"]["routerServiceAccountUid"] = fresh["metadata"]["uid"]
        assert_fresh_private_identity(h, reg)
        for uid in (original["metadata"]["uid"], "unregistered"):
            h.objects["serviceaccount", "sre-api-router"]["metadata"]["uid"] = uid
            with self.assertRaises(AssertionError):
                assert_fresh_private_identity(h, reg)


if __name__ == "__main__":
    unittest.main()
