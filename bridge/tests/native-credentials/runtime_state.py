"""Observe the controlled runtime over the existing permitted gateway port."""

import http.client
import json
import subprocess

from native_api import ROOT, require
from private_tls import forward


def assert_ephemeral_workspace(pod):
    spec = pod.get("spec", {})
    agents = [container for container in spec.get("containers", []) if container["name"] == "openclaw"]
    require(len(agents) == 1, "Native workspace fixture has no unique agent container")
    mounts = [mount for mount in agents[0].get("volumeMounts", []) if mount["mountPath"] == "/sandbox"]
    require(len(mounts) == 1 and not mounts[0].get("readOnly")
            and not mounts[0].get("subPath") and not mounts[0].get("subPathExpr"),
            "Native workspace fixture must mount the writable sandbox volume directly")
    volumes = [volume for volume in spec.get("volumes", []) if volume["name"] == mounts[0]["name"]]
    require(len(volumes) == 1 and isinstance(volumes[0].get("emptyDir"), dict)
            and set(volumes[0]) <= {"name", "emptyDir"},
            "Native workspace fixture must retain the existing emptyDir contract")


def runtime_state(namespace, pod):
    with forward(namespace, f"pod/{pod}", 18790, 18789):
        connection = http.client.HTTPConnection("127.0.0.1", 18790, timeout=10)
        try:
            connection.request("GET", "/native-proof")
            response = connection.getresponse()
            body = response.read(4096)
            require(response.status == 200 and len(body) < 4096,
                    "Controlled runtime proof unavailable")
            proof = json.loads(body)
            require(proof.get("uid") == 1000, "Runtime proof did not execute as the protected agent UID")
            return proof
        finally:
            connection.close()


def assert_agent_exec_denied(namespace, pod):
    result = subprocess.run([
        "kubectl", "exec", "-n", namespace, pod, "-c", "openclaw", "--", "true",
    ], cwd=ROOT, capture_output=True, text=True, timeout=30, check=False)
    require(result.returncode != 0 and "kars-sandbox-exec-ban" in result.stderr
            and "Forbidden" in result.stderr,
            "Native exec rejection did not come from the intended agent boundary")
