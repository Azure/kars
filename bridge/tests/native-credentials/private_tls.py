"""Pinned local TLS transport for API tests, not NetworkPolicy evidence."""

from contextlib import contextmanager
import http.client
import json
import socket
import ssl
import subprocess

from native_api import ROOT, STATE, require, until


class PinnedConnection(http.client.HTTPConnection):
    def __init__(self, port, name, ca):
        super().__init__("127.0.0.1", port, timeout=20)
        self.name = name
        self.context = ssl.create_default_context(cadata=ca)

    def connect(self):
        raw = socket.create_connection((self.host, self.port), self.timeout)
        try:
            self.sock = self.context.wrap_socket(raw, server_hostname=self.name)
        except BaseException:
            raw.close()
            raise


def call(port, endpoint, token, method, path, body=None, scope=None):
    connection = PinnedConnection(port, endpoint["serverName"], endpoint["caPem"])
    headers = {"Authorization": f"Bearer {token}", "Content-Type": "application/json"}
    if scope is not None:
        headers["x-kars-service-scope"] = scope
    try:
        connection.request(method, path, None if body is None else json.dumps(body), headers)
        response = connection.getresponse()
        raw = response.read(65536)
        require(len(raw) < 65536, "Private TLS API response exceeded bound")
        require(response.status not in (301, 302, 303, 307, 308), "Private API attempted redirect")
        value = json.loads(raw) if raw and response.getheader("Content-Type", "").startswith("application/json") else None
        return response.status, value
    finally:
        connection.close()


@contextmanager
def forward(namespace, target, local, remote):
    with (STATE / f"forward-{local}.log").open("w") as output:
        process = subprocess.Popen([
            "kubectl", "port-forward", "-n", namespace, target,
            f"{local}:{remote}", "--address=127.0.0.1",
        ], cwd=ROOT, stdin=subprocess.DEVNULL, stdout=output, stderr=output)
        try:
            def listening():
                require(process.poll() is None, "Private API port-forward terminated")
                try:
                    with socket.create_connection(("127.0.0.1", local), 1):
                        return True
                except OSError:
                    return False
            until("private TLS forwarding listener", listening, 30)
            yield
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
