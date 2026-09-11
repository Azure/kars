# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Real kubectl filter checks against a credential-free loopback API fixture."""

import json
import os
from pathlib import Path
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import unittest
from unittest.mock import patch

from sre_authority import registration_schema as api


class ConnectionProxyTests(unittest.TestCase):
    def test_rejects_unowned_or_unbounded_namespace_arguments(self):
        for namespaces in ((), ("kars-system", "kars-system-ordinary"),
                           ("kars-cel-" + "a" * 32, "other"),
                           ("kars-cel-.*", "kars-cel-.*-ordinary")):
            with self.subTest(namespaces=namespaces), self.assertRaises(RuntimeError):
                api.connection_proxy_arguments(namespaces)

    def test_default_filter_stays_closed_and_exception_only_reaches_absent_fixture_paths(self):
        namespace = "kars-cel-" + "a" * 32
        ordinary = namespace + "-ordinary"
        requests = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                requests.append(("GET", self.path))
                body = ({"major": "1", "minor": "31", "gitVersion": "v1.31.0"}
                        if self.path == "/version" else {
                            "apiVersion": "v1", "kind": "Status", "status": "Failure",
                            "code": 404, "reason": "NotFound",
                            "details": {"name": "phase-connect-absent", "kind": "pods"},
                        })
                self.send_response(200 if self.path == "/version" else 404)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps(body).encode())

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory(prefix="kars-connection-proxy-") as directory:
                root = Path(directory)
                config = root / "kubeconfig"
                config.write_text(json.dumps({
                    "apiVersion": "v1", "kind": "Config", "current-context": api.CONTEXT,
                    "clusters": [{"name": "owned", "cluster": {
                        "server": f"http://127.0.0.1:{server.server_port}"}}],
                    "users": [{"name": "empty", "user": {}}],
                    "contexts": [{"name": api.CONTEXT, "context": {
                        "cluster": "owned", "user": "empty"}}],
                }))
                config.chmod(0o600)
                path = f"/api/v1/namespaces/{namespace}/pods/phase-connect-absent/exec"
                with patch.dict(os.environ, {"KUBECONFIG": str(config)}):
                    with api.kind_proxy(root) as (port, _):
                        self.assertEqual(api.request(port, "GET", path)[0], 403)
                    self.assertNotIn(("GET", path), requests)
                    with api.kind_proxy(root, connection_namespaces=(namespace, ordinary)) as (port, _):
                        for ns in (namespace, ordinary):
                            for verb in ("exec", "attach", "portforward", "proxy"):
                                allowed = f"/api/v1/namespaces/{ns}/pods/phase-connect-absent/{verb}"
                                code, body = api.request(port, "GET", allowed)
                                self.assertEqual(code, 404)
                                self.assertEqual(body["reason"], "NotFound")
                                self.assertIn(("GET", allowed), requests)
                        before = list(requests)
                        for forbidden in (
                            f"/api/v1/namespaces/{namespace}/pods/existing/exec",
                            "/api/v1/namespaces/other/pods/phase-connect-absent/exec",
                            f"/api/v1/namespaces/{namespace}/secrets",
                        ):
                            self.assertEqual(api.request(port, "GET", forbidden)[0], 403)
                        self.assertEqual(api.request(port, "POST", path, {})[0], 403)
                        self.assertEqual(requests, before)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
