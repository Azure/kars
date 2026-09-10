# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Managed MCP fixture checks, not native Kubernetes acceptance."""

import importlib.util
import contextlib
import io
import json
from pathlib import Path
import unittest
from unittest.mock import MagicMock, patch

SPEC = importlib.util.spec_from_file_location(
    "managed_mcp_fixture", Path(__file__).with_name("managed-mcp.py"))
MCP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MCP)


class ManagedMcpTests(unittest.TestCase):
    def test_rpc_advertises_both_required_media_types_without_changing_scope(self):
        response = MagicMock()
        response.status = 200
        response.read.return_value = b'{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}'
        opener = MagicMock()
        opener.open.return_value.__enter__.return_value = response
        with patch.object(MCP.urllib.request, "build_opener", return_value=opener):
            status, body = MCP.rpc(12345, "tools/list", {}, MCP.SERVER)
        request = opener.open.call_args.args[0]
        headers = {key.lower(): value for key, value in request.header_items()}
        self.assertEqual(headers, {
            "content-type": "application/json",
            "accept": "application/json, text/event-stream",
            "x-kars-mcp-server": MCP.SERVER,
        })
        self.assertEqual(request.full_url, "http://127.0.0.1:12345/mcp")
        self.assertEqual(json.loads(request.data), {
            "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
        })
        self.assertEqual(opener.open.call_args.kwargs, {"timeout": 15})
        response.read.assert_called_once_with(2 * 1024 * 1024)
        self.assertEqual(status, 200)
        self.assertEqual(body["result"]["tools"], [])

    def test_catalog_facts_never_copy_payloads_headers_or_tool_names(self):
        secret = "PRIVATE_DIAGNOSTIC_SENTINEL"
        body = {"result": {"tools": [
            {"name": secret, "description": secret},
            {"name": "e2e_tools.echo"},
        ]}, "error": {"code": -32000, "message": secret, "data": {"token": secret}},
            "headers": {"Authorization": secret}}
        facts = MCP.catalog_facts(200, body)
        self.assertEqual(facts, {"stage": "router_catalog", "httpStatus": 200,
            "rpcError": True, "rpcErrorCode": -32000, "toolCount": 2,
            "expectedToolPresent": True})
        self.assertNotIn(secret, json.dumps(facts))
        self.assertNotIn("Authorization", json.dumps(facts))

    def test_catalog_facts_handle_malformed_envelopes_without_logging_values(self):
        for body in (None, [], "PRIVATE", {"result": []}, {"result": {"tools": "PRIVATE"}},
                     {"result": {"tools": [None, "PRIVATE", {}]}},
                     {"error": {"code": "PRIVATE", "message": "PRIVATE"}}):
            with self.subTest(body=type(body).__name__):
                facts = MCP.catalog_facts(None, body)
                self.assertIsNone(facts["httpStatus"])
                self.assertFalse(facts["expectedToolPresent"])
                self.assertIsNone(facts["rpcErrorCode"])
                self.assertNotIn("PRIVATE", json.dumps(facts))

    def test_catalog_timeout_retains_status_and_keeps_the_native_deadline(self):
        process = MagicMock()
        process.poll.return_value = None

        def exhausted(label, predicate, seconds):
            self.assertEqual(label, "actual routed MCP catalog after reconciliation")
            self.assertEqual(seconds, 120)
            self.assertFalse(predicate())
            raise AssertionError(f"{label} did not converge")

        for status in (403, 404, 406, 503):
            output = io.StringIO()
            with self.subTest(status=status), \
                    patch.object(MCP, "rpc", return_value=(status, None)), \
                    patch.object(MCP, "wait", side_effect=exhausted), \
                    contextlib.redirect_stdout(output):
                with self.assertRaisesRegex(AssertionError, "did not converge"):
                    MCP.wait_for_catalog(12345, process)
            facts = json.loads(output.getvalue().removeprefix("MCP-DIAG "))
            self.assertEqual(facts["httpStatus"], status)
            self.assertEqual(facts["stage"], "router_catalog")
            self.assertTrue(facts["portForwardAlive"])

    def test_catalog_transport_failure_diagnostics_never_copy_exception_text(self):
        process = MagicMock()
        process.poll.return_value = None

        def exhausted(_label, predicate, seconds):
            self.assertEqual(seconds, 120)
            self.assertFalse(predicate())
            raise AssertionError("catalog did not converge")

        for error, classification in ((OSError("PRIVATE"), "io"), (ValueError("PRIVATE"), "invalid_json")):
            output = io.StringIO()
            with patch.object(MCP, "rpc", side_effect=error), \
                    patch.object(MCP, "wait", side_effect=exhausted), \
                    contextlib.redirect_stdout(output):
                with self.assertRaisesRegex(AssertionError, "did not converge"):
                    MCP.wait_for_catalog(12345, process)
            facts = json.loads(output.getvalue().removeprefix("MCP-DIAG "))
            self.assertEqual(facts["transportError"], classification)
            self.assertNotIn("PRIVATE", output.getvalue())

    def test_catalog_success_still_requires_exact_scoped_tool_and_no_rpc_error(self):
        process = MagicMock()
        process.poll.return_value = None
        good = {"result": {"tools": [{"name": "e2e_tools.echo"}]}}
        responses = [
            OSError("connection not ready"),
            (200, {"result": {"tools": [{"name": "other.echo"}]}}),
            (200, {**good, "error": {"code": -32000}}),
            (200, good),
        ]

        def converge(_label, predicate, seconds):
            self.assertEqual(seconds, 120)
            for _ in range(3):
                self.assertFalse(predicate())
            return predicate()

        output = io.StringIO()
        with patch.object(MCP, "rpc", side_effect=responses), \
                patch.object(MCP, "wait", side_effect=converge), \
                contextlib.redirect_stdout(output):
            self.assertEqual(MCP.wait_for_catalog(12345, process), good)
        self.assertEqual(output.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
