# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
import copy
import json
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

import witness
from test_witness import Api, dns, metadata, tcp


class FleetApi(Api):
    def __init__(self, names=("demo", "mission-worker", "team-worker")):
        super().__init__()
        self.inventory = {name: {"metadata": metadata(name), "spec": {
            "networkPolicy": {"egressMode": "Strict"}}} for name in names}
        self.baseline_errors = {}
        self.page_size = 2
        self.version = "123"

    def request(self, path, method="GET", body=None):
        if "/karssandboxes?" in path:
            self.calls.append((path, method, body))
            query = parse_qs(urlsplit(path).query)
            assert query["limit"] == ["250"]
            offset = int(query.get("continue", ["0"])[0])
            values = list(self.inventory.values())
            end = offset + self.page_size
            return {"metadata": {"resourceVersion": self.version,
                                 "continue": str(end) if end < len(values) else ""},
                    "items": copy.deepcopy(values[offset:end])}
        if "/configmaps/karssandbox-" in path:
            self.calls.append((path, method, body))
            name = path.split("/configmaps/karssandbox-")[1].removesuffix("-egress-allowlist")
            error = self.baseline_errors.get(name)
            if isinstance(error, Exception):
                raise error
            if error is not None:
                return copy.deepcopy(error)
            return {"metadata": metadata(f"karssandbox-{name}-egress-allowlist", f"kars-{name}"),
                    "data": {"allowlist.json": json.dumps({"schemaVersion": 1, "endpoints": []})}}
        return super().request(path, method, body)


def config():
    return {"scope": "all", "sandboxes": [], "window_seconds": 5, "dns_image": "dns", "tcp_image": "tcp"}


class FleetTests(unittest.TestCase):
    def test_discovery_paginates_without_the_selected_mode_fifty_sandbox_cap(self):
        api = FleetApi([f"worker-{n}" for n in range(71)])
        api.page_size = 25
        result = witness.discovery(api)
        self.assertEqual(len(result), 71)
        self.assertEqual(len(api.calls), 3)
        self.assertEqual(result["worker-70"]["metadata"]["uid"], "worker-70-uid")
        self.assertTrue(all("/namespaces/kars-system/karssandboxes?limit=250" in call[0] for call in api.calls))

    def test_capture_uses_all_discovered_namespaces_not_unrelated_namespaces(self):
        api = FleetApi()
        with patch.object(witness, "capture", return_value=[]) as capture:
            report = witness.sample(api, config())
        self.assertEqual(report["status"], "empty")
        self.assertEqual({row["sandbox"] for row in report["sandboxes"]}, set(api.inventory))
        for call in capture.call_args_list:
            self.assertEqual(call.args[2:], (["node-1"], ["kars-demo", "kars-mission-worker", "kars-team-worker"]))
        self.assertTrue(all(row["sandbox_uid"] == row["sandbox"] + "-uid" for row in report["sandboxes"]))

    def test_next_cycle_automatically_discovers_new_worker_and_removes_deleted_worker(self):
        api = FleetApi(["demo"])
        with patch.object(witness, "capture", return_value=[]):
            self.assertEqual(len(witness.sample(api, config())["sandboxes"]), 1)
            api.inventory["new-worker"] = {"metadata": metadata("new-worker"), "spec": {}}
            self.assertEqual(len(witness.sample(api, config())["sandboxes"]), 2)
            del api.inventory["demo"]
            rows = witness.sample(api, config())["sandboxes"]
            self.assertEqual([row["sandbox"] for row in rows], ["new-worker"])

    def test_empty_fleet_never_starts_an_unfiltered_capture(self):
        api = FleetApi([])
        with patch.object(witness, "capture") as capture, patch.object(witness.time, "sleep") as sleep:
            report = witness.sample(api, config())
        capture.assert_not_called()
        sleep.assert_called_once_with(5)
        self.assertEqual((report["status"], report["sandboxes"], report["event_count"]), ("empty", [], 0))

    def test_unavailable_baseline_does_not_erase_other_sandboxes_or_invent_compliance(self):
        for error in (witness.WitnessError("api_http_403"), witness.WitnessError("api_http_404"),
                      witness.WitnessError("api_unavailable_or_invalid")):
            with self.subTest(error=str(error)):
                api = FleetApi()
                api.baseline_errors["team-worker"] = error
                def capture(image, *_):
                    return [dns(namespace="kars-team-worker"), dns()] if image == "dns" else [tcp()]
                with patch.object(witness, "capture", side_effect=capture):
                    report = witness.sample(api, config())
                rows = {row["sandbox"]: row for row in report["sandboxes"]}
                self.assertEqual(report["event_count"], 3)
                self.assertEqual(rows["demo"]["verdict"], "BEYOND-DECLARED")
                row = rows["team-worker"]
                self.assertEqual(row["verdict"], "UNAVAILABLE")
                self.assertEqual(row["declaration_diagnostic"], "baseline_unavailable")
                self.assertIsNone(row["egress_mode"])
                self.assertEqual(row["observed_dns"], ["example.com"])
                for key in ("declared_hosts", "beyond_declared", "unused_declared"):
                    self.assertEqual(row[key], [])

    def test_malformed_and_deleting_baselines_are_explicitly_unavailable(self):
        api = FleetApi()
        api.baseline_errors["team-worker"] = {"metadata": metadata("wrong", "kars-team-worker")}
        api.inventory["mission-worker"]["metadata"]["deletionTimestamp"] = "2026-01-01T00:00:00Z"
        with patch.object(witness, "capture", return_value=[]):
            rows = {r["sandbox"]: r for r in witness.sample(api, config())["sandboxes"]}
        self.assertEqual(rows["team-worker"]["declaration_diagnostic"], "declaration_invalid")
        self.assertEqual(rows["mission-worker"]["declaration_diagnostic"], "baseline_unavailable")
        self.assertEqual(rows["demo"]["verdict"], "NO-TRAFFIC")

    def test_uid_replacement_addition_and_deletion_during_capture_invalidate_the_sample(self):
        for change in ("replace", "add", "delete", "policy"):
            api = FleetApi(["demo"])
            def capture(image, *_):
                if image == "dns":
                    if change == "replace":
                        api.inventory["demo"]["metadata"]["uid"] = "new-uid"
                    elif change == "add":
                        api.inventory["new"] = {"metadata": metadata("new"), "spec": {}}
                    elif change == "policy":
                        api.inventory["demo"]["spec"]["networkPolicy"]["egressMode"] = "Learn"
                    else:
                        del api.inventory["demo"]
                return []
            with self.subTest(change=change), patch.object(witness, "capture", side_effect=capture):
                with self.assertRaisesRegex(witness.WitnessError, "sandbox_inventory_changed_during_capture"):
                    witness.sample(api, config())

    def test_status_churn_does_not_invalidate_attribution(self):
        api = FleetApi(["demo"])
        def capture(image, *_):
            if image == "dns":
                api.inventory["demo"]["metadata"]["resourceVersion"] = "200"
                api.inventory["demo"]["status"] = {"phase": "Running"}
                api.version = "200"
            return []
        with patch.object(witness, "capture", side_effect=capture):
            self.assertEqual(witness.sample(api, config())["status"], "empty")

    def test_malformed_or_inconsistent_list_fails_closed(self):
        base = {"metadata": {"resourceVersion": "1"}, "items": [{"metadata": metadata("demo"), "spec": {}}]}
        cases = [{}, dict(base, items="not-list"), dict(base, metadata={})]
        for field, value in (("name", "../other"), ("namespace", "other"), ("uid", "")):
            case = copy.deepcopy(base)
            case["items"][0]["metadata"][field] = value
            cases.append(case)
        for case in cases:
            with self.subTest(case=case), patch.object(Api, "request", return_value=case):
                with self.assertRaisesRegex(witness.WitnessError, "discovery_invalid"):
                    witness.discovery(Api())
        first = dict(base, metadata={"resourceVersion": "1", "continue": "next &/"})
        for second in (first, dict(base, metadata={"resourceVersion": "2"}), base):
            with patch.object(Api, "request", side_effect=[first, second]) as request:
                with self.assertRaisesRegex(witness.WitnessError, "discovery_invalid"):
                    witness.discovery(Api())
                self.assertTrue(request.call_args.args[0].endswith("continue=next%20%26%2F"))

    def test_discovery_is_bounded_not_silently_truncated(self):
        with patch.object(witness, "MAX_OUTPUT", 1):
            with self.assertRaisesRegex(witness.WitnessError, "discovery_output_limit"):
                witness.discovery(FleetApi())
        with patch.object(witness, "STOP", True):
            with self.assertRaisesRegex(witness.WitnessError, "discovery_interrupted_or_expired"):
                witness.discovery(FleetApi())
        with patch.object(Api, "request", side_effect=witness.WitnessError("api_http_410")):
            with self.assertRaisesRegex(witness.WitnessError, "api_http_410"):
                witness.discovery(Api())

    def test_scope_is_unambiguous_and_legacy_selected_does_not_list(self):
        for overrides in ({"scope": "unexpected"}, {"sandboxes": ["demo"]}):
            with self.subTest(overrides=overrides):
                with self.assertRaisesRegex(witness.WitnessError, "sandbox_scope_invalid"):
                    witness.sample(Api(), dict(config(), **overrides))
        selected = dict(config(), sandboxes=["demo"])
        del selected["scope"]
        api = Api()
        with patch.object(witness, "capture", return_value=[]):
            self.assertEqual(witness.sample(api, selected)["status"], "empty")
        self.assertFalse(any("/karssandboxes?" in call[0] for call in api.calls))


if __name__ == "__main__":
    unittest.main()
