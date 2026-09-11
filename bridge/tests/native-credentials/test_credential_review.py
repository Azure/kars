"""Operator protocol orchestration tests, not native delivery evidence."""

import copy
import unittest

from credential_review import REVIEW, WRITE, reviewed_credential_write
from native_api import Failure, require

PRIVATE = "PRIVATE_VALUE_NEVER_REPORTED"
REQUEST = {"namespace": "work", "kind": "KarsSandbox", "target": "target",
           "targetUid": "target-uid", "key": "SLACK_BOT_TOKEN", "value": PRIVATE}
GRANT = {"metadata": {"uid": "grant-uid", "generation": 1}}


def review():
    return {"token": "initial-review", "expiresAt": 5000, "submission": 1,
            "continuation": False, "bindingOnly": False, "metadata": {
                "target": {"namespace": "work", "kind": "KarsSandbox", "name": "target",
                           "uid": "target-uid", "generation": 1, "version": "1", "intent": "target-intent"},
                "grant": {"uid": "grant-uid", "generation": 1, "version": "1",
                          "intent": "grant-intent", "workspaceUid": "workspace-uid", "legacyInventory": "legacy"},
                "source": {"name": "kars-credential-input-sandbox-target", "uid": None,
                           "version": None, "metadataDigest": None, "keys": []},
                "key": "SLACK_BOT_TOKEN"}}


def receipt(stored=True):
    return {"token": "owned-continuation", "outcome": "source-stored" if stored else "no-write-attempted",
            "source": {"name": "kars-credential-input-sandbox-target", "uid": "source-uid",
                       "version": "1", "metadataDigest": "source-intent"} if stored else None}


class BffFixture:
    def __init__(self, stored=True):
        self.calls = []
        self.stored = stored
        self.failures = 1
        self.failure_status = 409
        self.proof = True
        self.alter = lambda _: None
        self.current = review()
        self.writes = 0
        self.transport = False
        self.reject_refresh = False

    def call(self, method, path, body, expected=200, include_status=False):
        self.calls.append((method, path, copy.deepcopy(body)))
        if path == REVIEW:
            require("value" not in body, "Review must never receive the credential value")
            if body.get("continuation"):
                require(body["continuation"] == "owned-continuation", "Wrong owned proof")
                if self.reject_refresh:
                    self.alter(self.current)
                    return 409, {"error": {"code": "conflict"}}
                self.current["continuation"] = True
                self.current["bindingOnly"] = self.stored
                self.current["submission"] += 1
                self.current["metadata"]["target"]["version"] = str(self.current["submission"])
                self.current["metadata"]["grant"]["version"] = str(self.current["submission"])
                if self.stored:
                    self.current["metadata"]["source"] = {
                        **receipt()["source"], "keys": ["SLACK_BOT_TOKEN"]}
                self.alter(self.current)
            current = copy.deepcopy(self.current)
            return (200, current) if include_status else current
        require(path == WRITE and include_status, "Unexpected protocol path")
        self.writes += 1
        if self.transport:
            raise ConnectionError("fixed transport failure")
        if self.failures:
            self.failures -= 1
            require(self.failure_status in expected, "Unexpected BFF failure")
            error = {"code": "conflict"}
            if self.proof:
                error["credentialContinuation"] = receipt(self.stored)
            return self.failure_status, {"error": error}
        return 200, {"stored": True, "source": {"name": self.current["metadata"]["source"]["name"],
                                               "uid": "source-uid"}}


class CredentialReviewTests(unittest.TestCase):
    def run_protocol(self, bff):
        facts = []
        result = reviewed_credential_write(bff, REQUEST, GRANT, 1, facts.append)
        self.assertNotIn(PRIVATE, str(facts))
        return result, facts

    def test_acknowledged_partial_write_and_no_write_need_separate_review_then_resubmit(self):
        for stored in (False, True):
            with self.subTest(stored=stored):
                bff = BffFixture(stored)
                result, facts = self.run_protocol(bff)
                self.assertTrue(result["stored"])
                self.assertEqual([path for _, path, _ in bff.calls], [REVIEW, WRITE, REVIEW, WRITE])
                self.assertEqual([f["httpStatus"] for f in facts if "httpStatus" in f], [409, 200])
                self.assertEqual(bff.calls[1][2]["value"], bff.calls[3][2]["value"])

    def test_first_success_does_not_add_a_second_write_or_review(self):
        bff = BffFixture()
        bff.failures = 0
        self.run_protocol(bff)
        self.assertEqual([path for _, path, _ in bff.calls], [REVIEW, WRITE])

    def test_generic_failure_collision_and_lost_ack_are_not_retry_authority(self):
        for mode in ("502", "403", "collision", "transport"):
            with self.subTest(mode=mode):
                bff = BffFixture()
                if mode in ("502", "403"):
                    bff.failure_status = int(mode)
                if mode == "collision":
                    bff.proof = False
                if mode == "transport":
                    bff.transport = True
                with self.assertRaises((Failure, ConnectionError)):
                    self.run_protocol(bff)
                self.assertEqual([path for _, path, _ in bff.calls], [REVIEW, WRITE])

    def test_every_review_fence_is_checked_before_the_next_write(self):
        for area, field, value in [
            ("target", "uid", "replacement"), ("target", "generation", 2),
            ("target", "intent", "changed"), ("grant", "uid", "replacement"),
            ("grant", "generation", 2), ("grant", "intent", "changed"),
            ("grant", "workspaceUid", "replacement"), ("grant", "legacyInventory", "changed"),
            ("source", "uid", "replacement"), ("source", "version", "2"),
            ("source", "metadataDigest", "changed"), ("source", "keys", ["OTHER_KEY"]),
        ]:
            with self.subTest(area=area, field=field):
                bff = BffFixture()
                bff.alter = lambda current: current["metadata"][area].__setitem__(field, value)
                with self.assertRaises(Failure):
                    self.run_protocol(bff)
                self.assertEqual(bff.writes, 1)

    def test_unwritten_receipt_cannot_adopt_a_newly_appearing_source(self):
        bff = BffFixture(False)
        bff.alter = lambda current: current["metadata"]["source"].update(
            uid="foreign", version="1", metadataDigest="foreign")
        with self.assertRaises(Failure):
            self.run_protocol(bff)
        self.assertEqual(bff.writes, 1)

    def test_three_submissions_is_the_bound_even_with_new_owned_receipts(self):
        bff = BffFixture()
        bff.failures = 10
        with self.assertRaises(Failure):
            self.run_protocol(bff)
        self.assertEqual(bff.writes, 3)
        self.assertEqual(len(bff.calls), 6)

    def test_expiry_cannot_be_extended_by_re_review(self):
        bff = BffFixture()
        bff.alter = lambda current: current.__setitem__("expiresAt", 6000)
        with self.assertRaises(Failure):
            self.run_protocol(bff)
        self.assertEqual(bff.writes, 1)

    def test_rejected_refresh_records_only_change_flags_and_never_submits_fresh_ticket(self):
        bff = BffFixture(False)
        bff.reject_refresh = True
        bff.alter = lambda current: current["metadata"]["target"].update(intent="new-private-intent")
        facts = []
        with self.assertRaises(Failure):
            reviewed_credential_write(bff, REQUEST, GRANT, 1, facts.append)
        self.assertEqual(bff.writes, 1)
        self.assertEqual([path for _, path, _ in bff.calls], [REVIEW, WRITE, REVIEW, REVIEW])
        self.assertFalse(facts[-1]["unchanged"]["target"]["intent"])
        self.assertTrue(facts[-1]["unchanged"]["target"]["uid"])
        self.assertFalse(facts[-1]["writeResubmitted"])
        for secret in (PRIVATE, "new-private-intent", "initial-review", "owned-continuation"):
            self.assertNotIn(secret, str(facts))


if __name__ == "__main__":
    unittest.main()
