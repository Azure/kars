# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Exercise the actual embedded startup program without loading another MeshClient."""

import contextlib
import http.client
import io
from pathlib import Path
import sys
import threading
import time
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import Mock, patch


ENTRYPOINT = Path(__file__).resolve().parents[3] / "sandbox-images/hermes/entrypoint.sh"


class MeshEntrypointTests(unittest.TestCase):
    def run_startup(self, outcomes, *, connect_error=None):
        source = ENTRYPOINT.read_text()
        program = source.split('      python3 -c "\n', 1)[1].split('\n" 2>&1 &', 1)[0]
        clock = [0.0]
        requests = []
        connections = []
        pending = iter(outcomes)
        last = [outcomes[-1]]
        ready = [False]
        mesh = SimpleNamespace()

        def connect():
            self.assertTrue(ready[0], "Mesh registration ran before router readiness")
            if connect_error:
                raise connect_error
            return object()

        mesh._get_or_init_client = Mock(side_effect=connect)
        worker = SimpleNamespace(start_worker=Mock())
        plugin = ModuleType("kars_runtime_hermes.plugin")
        plugin.mesh, plugin.mesh_worker = mesh, worker

        def connection(host, port, timeout):
            self.assertEqual((host, port), ("127.0.0.1", 8443))
            self.assertGreater(timeout, 0)
            self.assertLessEqual(timeout, 2)
            current = next(pending, last[0])
            last[0] = current
            conn = Mock()

            def request(method, path):
                requests.append((method, path))
                self.assertEqual((method, path), ("GET", "/healthz"))
                if isinstance(current, BaseException):
                    raise current

            def response():
                ready[0] = current == 200
                return SimpleNamespace(status=current)

            conn.request.side_effect = request
            conn.getresponse.side_effect = response
            connections.append(conn)
            return conn

        output = io.StringIO()
        exit_code = None
        with (patch.dict(sys.modules, {"kars_runtime_hermes.plugin": plugin}),
              patch.object(http.client, "HTTPConnection", side_effect=connection),
              patch.object(time, "monotonic", side_effect=lambda: clock[0]),
              patch.object(time, "sleep", side_effect=lambda seconds: clock.__setitem__(0, clock[0] + seconds)),
              patch.object(threading, "Event") as event,
              contextlib.redirect_stdout(output)):
            try:
                exec(compile(program, str(ENTRYPOINT), "exec"), {})
            except SystemExit as error:
                exit_code = error.code
        for conn in connections:
            conn.close.assert_called_once()
        return SimpleNamespace(mesh=mesh, worker=worker, event=event, output=output.getvalue(),
                               exit_code=exit_code, requests=requests, elapsed=clock[0])

    def test_delayed_sidecar_connects_once_after_refusals_and_unhealthy_status(self):
        result = self.run_startup([ConnectionRefusedError(), 503, 200])
        self.assertIsNone(result.exit_code)
        self.assertEqual(len(result.requests), 3)
        result.mesh._get_or_init_client.assert_called_once_with()
        result.worker.start_worker.assert_called_once_with(result.mesh._get_or_init_client)
        result.event.return_value.wait.assert_called_once_with()

    def test_healthy_sidecar_does_not_delay_registration(self):
        result = self.run_startup([200])
        self.assertEqual(result.elapsed, 0)
        result.mesh._get_or_init_client.assert_called_once_with()

    def test_unavailable_sidecar_expires_without_identity_initialization(self):
        result = self.run_startup([ConnectionRefusedError()])
        self.assertEqual(result.exit_code, 1)
        self.assertEqual(result.elapsed, 60)
        result.mesh._get_or_init_client.assert_not_called()
        result.worker.start_worker.assert_not_called()
        self.assertIn("FATAL: TimeoutError", result.output)

    def test_authentication_failure_is_not_retried_or_exposed(self):
        result = self.run_startup([200], connect_error=ValueError("private-token-must-not-be-logged"))
        self.assertEqual(result.exit_code, 1)
        result.mesh._get_or_init_client.assert_called_once_with()
        result.worker.start_worker.assert_not_called()
        self.assertIn("FATAL: ValueError", result.output)
        self.assertNotIn("private-token", result.output)

    def test_keepalive_logs_are_visible_without_private_pod_exec(self):
        source = ENTRYPOINT.read_text()
        self.assertNotIn("hermes-mesh-keepalive.log", source)
        self.assertIn('\n" 2>&1 &', source)


if __name__ == "__main__":
    unittest.main()
