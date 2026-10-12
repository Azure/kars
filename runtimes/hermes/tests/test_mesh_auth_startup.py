# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute the production auth block, not privileged setup or gateway startup."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys

import pytest


ROOT = Path(__file__).resolve().parents[3]
ENTRYPOINT = ROOT / "sandbox-images/hermes/entrypoint.sh"
AUTH_BLOCK = ENTRYPOINT.read_text().split(
    "# ── Workload Identity → Entra token exchange", 1
)[1].split("# ── Contract version assertion", 1)[0]
AUTH_BLOCK = "# ── Workload Identity → Entra token exchange" + AUTH_BLOCK


def run_auth(tmp_path, *, body='{"access_token":"test-token"}', status="200", env=None,
             exit_code=0, responses=None):
    environment = {
        "PATH": os.environ["PATH"],
        "PYTHONDONTWRITEBYTECODE": "1",
        "MESH_AUTH_BACKEND": "EntraAgentIdentity",
        "MESH_TOKEN_MAX_WAIT": "1",
        "AGT_TRUST_THRESHOLD": "75",
        "TEST_BODY": body,
        "TEST_STATUS": status,
        "TEST_EXIT": str(exit_code),
        "TEST_PYTHON": sys.executable,
        "TEST_CALLS": str(tmp_path / "calls"),
        "TEST_STATE": str(tmp_path / "state.json"),
        "TEST_RESPONSES": str(tmp_path / "responses.json"),
        **(env or {}),
    }
    (tmp_path / "responses.json").write_text(json.dumps(responses))
    # Functions shadow curl, sleep and python3 only for this shell process.
    # Every request is recorded; no network or real sleep is possible.
    harness = r'''
set -e
curl() {
  printf '%s\n' "$*" >> "$TEST_CALLS"
  "$TEST_PYTHON" -c '
import json, os, sys
from pathlib import Path
rows = json.loads(Path(os.environ["TEST_RESPONSES"]).read_text())
row = {"body": os.environ["TEST_BODY"], "status": os.environ["TEST_STATUS"],
       "exit": int(os.environ["TEST_EXIT"])}
if rows:
    n = len(Path(os.environ["TEST_CALLS"]).read_text().splitlines())
    row = rows[min(n - 1, len(rows) - 1)]
print(row["body"])
if row["status"]:
    print("__HTTP_STATUS__" + row["status"])
sys.exit(row.get("exit", 0))'
}
sleep() { :; }
python3() { "$TEST_PYTHON" "$@"; }
save_state() {
  "$TEST_PYTHON" -c '
import os, json
from pathlib import Path
Path(os.environ["TEST_STATE"]).write_text(json.dumps({
    k: os.environ.get(k) for k in ("AGT_OAUTH_TOKEN", "AGT_TRUST_THRESHOLD")
}))'
}
trap save_state EXIT
'''
    proc = subprocess.run(
        ["bash", "-c", harness + AUTH_BLOCK + '\nprintf "STARTUP_CONTINUES\\n"\n'],
        env=environment, cwd=ROOT, capture_output=True, text=True, timeout=15,
    )
    state = json.loads((tmp_path / "state.json").read_text())
    calls = (tmp_path / "calls").read_text().splitlines() if (tmp_path / "calls").exists() else []
    return proc, state, calls


def test_required_token_minted_without_lowering_trust(tmp_path):
    proc, state, calls = run_auth(tmp_path)
    assert proc.returncode == 0, proc.stderr
    assert "STARTUP_CONTINUES" in proc.stdout
    assert state == {"AGT_OAUTH_TOKEN": "test-token", "AGT_TRUST_THRESHOLD": "75"}
    assert len(calls) == 1
    assert "http://127.0.0.1:8443/v1/mesh-token" in calls[0]
    assert "--noproxy 127.0.0.1" in calls[0]
    assert "test-token" not in proc.stdout + proc.stderr


def test_required_provided_token_cannot_bypass_fresh_mint(tmp_path):
    proc, state, calls = run_auth(tmp_path, env={"AGT_OAUTH_TOKEN": "provided-token",
                                              "ROUTER_LOCAL_URL": "https://unused.example"})
    assert proc.returncode == 0
    assert state["AGT_OAUTH_TOKEN"] == "test-token"
    assert state["AGT_TRUST_THRESHOLD"] == "75"
    assert len(calls) == 1
    assert "http://127.0.0.1:8443/v1/mesh-token" in calls[0]


@pytest.mark.parametrize("status", ["301", "302", "401", "403", "404", "429", "500", "503", "000", ""])
def test_required_http_and_network_failures_stop_startup(tmp_path, status):
    proc, state, calls = run_auth(tmp_path, status=status, exit_code=7 if status in {"000", ""} else 0)
    assert proc.returncode != 0
    assert "STARTUP_CONTINUES" not in proc.stdout
    assert "refusing anonymous mesh startup" in proc.stderr
    assert state == {"AGT_OAUTH_TOKEN": None, "AGT_TRUST_THRESHOLD": "75"}
    assert len(calls) == 1


@pytest.mark.parametrize("body", [
    "not-json", "[]", "null", "{}", '{"access_token":null}', '{"access_token":true}',
    '{"access_token":1}', '{"access_token":{}}', '{"access_token":[]}',
    '{"access_token":""}', '{"access_token":"  \\t\\n"}', '{"access_token":"one two"}',
])
def test_required_invalid_token_responses_stop_startup(tmp_path, body):
    proc, state, _ = run_auth(tmp_path, body=body)
    assert proc.returncode != 0
    assert "STARTUP_CONTINUES" not in proc.stdout
    assert state == {"AGT_OAUTH_TOKEN": None, "AGT_TRUST_THRESHOLD": "75"}


def test_required_blank_provided_token_must_be_replaced(tmp_path):
    proc, state, calls = run_auth(tmp_path, env={"AGT_OAUTH_TOKEN": " \n\t"}, status="503")
    assert proc.returncode != 0
    assert state["AGT_OAUTH_TOKEN"] is None
    assert len(calls) == 1


def test_required_transient_failure_retries_and_succeeds(tmp_path):
    proc, state, calls = run_auth(tmp_path, env={"MESH_TOKEN_MAX_WAIT": "2"}, responses=[
        {"body": "unavailable", "status": "503"},
        {"body": '{"access_token":"recovered-token"}', "status": "200"},
    ])
    assert proc.returncode == 0, proc.stderr
    assert state == {"AGT_OAUTH_TOKEN": "recovered-token", "AGT_TRUST_THRESHOLD": "75"}
    assert len(calls) == 2


@pytest.mark.parametrize("token", ["", "provided-token"])
def test_required_and_skip_conflict_even_with_token(tmp_path, token):
    proc, state, calls = run_auth(tmp_path, env={"AGT_SKIP_ENTRA": "1", "AGT_OAUTH_TOKEN": token})
    assert proc.returncode != 0
    assert "conflicting AGT_SKIP_ENTRA=1" in proc.stderr
    assert "STARTUP_CONTINUES" not in proc.stdout
    assert state["AGT_TRUST_THRESHOLD"] == "75"
    assert not calls


def test_explicit_no_entra_skip_clears_stale_token(tmp_path):
    proc, state, calls = run_auth(tmp_path, env={
        "MESH_AUTH_BACKEND": "", "AGT_SKIP_ENTRA": "1", "AGT_OAUTH_TOKEN": "stale-token",
    })
    assert proc.returncode == 0
    assert state == {"AGT_OAUTH_TOKEN": None, "AGT_TRUST_THRESHOLD": "0"}
    assert not calls


@pytest.mark.parametrize("sre", ["true", "True", "TRUE", "1", "yes", "YES"])
def test_sre_skips_all_mesh_auth_even_conflicting_mesh_settings(tmp_path, sre):
    proc, state, calls = run_auth(tmp_path, env={"SRE_ENABLED": sre, "AGT_SKIP_ENTRA": "1"})
    assert proc.returncode == 0
    assert "mesh disabled" in proc.stdout
    assert state["AGT_TRUST_THRESHOLD"] == "75"
    assert not calls


def test_legacy_optional_wi_fallback_is_unchanged(tmp_path):
    token_file = tmp_path / "federated-token"
    token_file.write_text("test-workload-token")
    proc, state, calls = run_auth(tmp_path, body="AADSTS500011", env={
        "MESH_AUTH_BACKEND": "", "AZURE_FEDERATED_TOKEN_FILE": str(token_file),
        "AZURE_CLIENT_ID": "test-client", "AZURE_TENANT_ID": "test-tenant",
    })
    assert proc.returncode == 0
    assert state == {"AGT_OAUTH_TOKEN": None, "AGT_TRUST_THRESHOLD": "0"}
    assert len(calls) == 1
