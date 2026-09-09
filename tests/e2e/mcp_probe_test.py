# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import contextlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import threading
import unittest

from mcp_probe import Client, MAX_BYTES, PROTOCOL, decode

PRIVATE = "DO-NOT-LOG-SESSION-OR-PRIVATE-RESULT"


@contextlib.contextmanager
def server(**options):
    state = {"calls": [], **options}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["content-length"])))
            state["calls"].append((body, dict(self.headers)))
            status = 200
            content = "application/json"
            value = {"jsonrpc": "2.0", "id": body.get("id"), "result": {"tools": [{"name": "e2e_tools.echo"}]}}
            if "text/event-stream" not in self.headers.get("Accept", ""):
                status, content, value = 406, "text/plain", PRIVATE
            elif body["method"] == "initialize":
                value["result"] = {"protocolVersion": state.get("protocol", PROTOCOL), "capabilities": {}}
            elif body["method"] == "notifications/initialized":
                status, value = 202, None
            elif self.headers.get("X-Kars-Mcp-Server") == "unmounted":
                status, value = 404, None
            elif state.get("call_error"):
                value = {"jsonrpc": "2.0", "id": body.get("id"), "error": {"code": -32000, "message": PRIVATE}}
            if state.get("wrong_id") and body["method"] == "tools/list":
                value["id"] = 9999
            wire = b"" if value is None else json.dumps(value).encode()
            if state.get("sse") and value is not None:
                content = "text/event-stream"
                wire = b"data: " + wire + b"\n\n"
            if state.get("oversize") and body["method"] == "tools/list":
                wire = b" " * (MAX_BYTES + 1)
            if state.get("truncate") and body["method"] == "tools/list":
                wire = b"{}"
            self.send_response(status)
            self.send_header("Content-Type", content)
            self.send_header("Content-Length", str(len(wire) + (2 if state.get("truncate") and body["method"] == "tools/list" else 0)))
            self.send_header("Connection", "close")
            if body["method"] == "initialize":
                self.send_header("Mcp-Session-Id", PRIVATE)
            self.end_headers()
            try:
                self.wfile.write(wire)
            except ConnectionError:
                pass
            self.close_connection = True

    http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    worker = threading.Thread(target=http.serve_forever, daemon=True)
    worker.start()
    try:
        yield Client(http.server_port, "e2e-tools"), state
    finally:
        http.shutdown()
        http.server_close()
        worker.join(timeout=5)


class McpProbeTests(unittest.TestCase):
    def test_real_http_negotiates_both_media_types_and_carries_session_protocol_and_scope(self):
        output = io.StringIO()
        with server() as (client, state), contextlib.redirect_stdout(output):
            status, result = client.call("tools/list", {})
            self.assertEqual(status, 200)
            self.assertEqual(result["result"]["tools"][0]["name"], "e2e_tools.echo")
            self.assertEqual(client.call("tools/list", {}, "unmounted")[0], 404)
        self.assertEqual([body["method"] for body, _ in state["calls"]],
                         ["initialize", "notifications/initialized", "tools/list", "tools/list"])
        for _, headers in state["calls"]:
            self.assertEqual(headers["Accept"], "application/json, text/event-stream")
            self.assertEqual(headers["Mcp-Protocol-Version"], PROTOCOL)
        self.assertEqual(state["calls"][2][1]["Mcp-Session-Id"], PRIVATE)
        self.assertEqual(state["calls"][2][1]["X-Kars-Mcp-Server"], "e2e-tools")
        self.assertEqual(state["calls"][3][1]["X-Kars-Mcp-Server"], "unmounted")
        self.assertNotIn(PRIVATE, output.getvalue())
        self.assertIn('"httpStatus": 404', output.getvalue())

    def test_real_sse_response_is_correlated_without_logging_results(self):
        output = io.StringIO()
        with server(sse=True) as (client, _), contextlib.redirect_stdout(output):
            status, result = client.call("tools/list", {})
            self.assertEqual(status, 200)
            self.assertIn("result", result)
        self.assertIn('"contentType": "sse"', output.getvalue())
        self.assertNotIn(PRIVATE, output.getvalue())

    def test_mismatched_id_oversize_and_truncated_transport_fail_closed(self):
        for option in ("wrong_id", "oversize", "truncate"):
            with self.subTest(option=option), server(**{option: True}) as (client, _), \
                 contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(AssertionError):
                    client.call("tools/list", {})

    def test_error_response_never_replays_a_tools_call_or_logs_its_body(self):
        output = io.StringIO()
        with server(call_error=True) as (client, state), contextlib.redirect_stdout(output):
            status, result = client.call("tools/call", {"name": "e2e_tools.echo", "arguments": {}})
            self.assertEqual(status, 200)
            self.assertIn("error", result)
        self.assertEqual(sum(body["method"] == "tools/call" for body, _ in state["calls"]), 1)
        self.assertNotIn(PRIVATE, output.getvalue())
        self.assertIn('"category": "rpc-error"', output.getvalue())

    def test_unsupported_protocol_does_not_claim_discovery(self):
        with server(protocol="unrecognized") as (client, state), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(client.call("tools/list", {}), (0, None))
        self.assertEqual(len(state["calls"]), 1)

    def test_decoder_rejects_incomplete_or_duplicate_sse_and_unknown_content_type(self):
        frame = json.dumps({"jsonrpc": "2.0", "id": 7, "result": {}}).encode()
        for body, kind in ((b"data: " + frame, "text/event-stream"),
                           ((b"data: " + frame + b"\n\n") * 2, "text/event-stream"),
                           (frame, "text/plain"),
                           (b'{"jsonrpc":"2.0","id":true,"result":{}}', "application/json"),
                           (b'{"jsonrpc":"2.0","id":7,"result":[]}', "application/json")):
            with self.assertRaises(ValueError):
                decode(body, kind, 7)


if __name__ == "__main__":
    unittest.main()
