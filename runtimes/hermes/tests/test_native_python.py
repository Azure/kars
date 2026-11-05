# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Unit coverage; real supervisor tests require the actual Linux Task sandbox."""

import base64
import copy
import json
import os
import sys
import time
from unittest.mock import patch

import pytest

from kars_runtime_hermes.plugin import native_python as python
from kars_runtime_hermes.plugin import mission_protocol as wire
from kars_runtime_hermes.plugin.mission_native import Measurements
from kars_runtime_hermes.plugin.mission_native_tools import NativeTools


SOURCE = "print('actual output')\n"
ARGS = {
    "entrypoint": "app.py",
    "files": [{"path": "scratch/app.py", "name": "app.py", "sha256": python.digest(SOURCE.encode())}],
}
IDENTITY = ["test-task", "test-run", "test-assignment", "call-1"]


@pytest.mark.parametrize(
    "mutation",
    [
        {"entrypoint": "../app.py"},
        {"entrypoint": "app.sh"},
        {"timeout_seconds": True},
        {"timeout_seconds": 61},
        {"argv": ["bad\0arg"]},
        {"argv": ["x" * 1025]},
        {"command": "python app.py"},
        {"files": []},
        {"files": [{"path": "scratch/app.py", "name": "../app.py", "sha256": ARGS["files"][0]["sha256"]}]},
        {"files": [ARGS["files"][0], ARGS["files"][0]]},
    ],
)
def test_rejects_unbounded_or_shell_arguments(mutation):
    with pytest.raises(ValueError):
        python.validate({**copy.deepcopy(ARGS), **mutation})


def test_literal_argv_and_source_identity_remain_exact():
    args = {**ARGS, "argv": ["$(touch do-not-create)", "; echo no-shell"]}
    python.validate(args)
    assert args["argv"][0] == "$(touch do-not-create)"
    assert json.loads(python.reserve_receipt([], args, IDENTITY)) == {"version": 1, "runs": []}
    with pytest.raises(ValueError, match="capacity"):
        python.reserve_receipt([{"stdout": "x" * 40000}], args, IDENTITY)


def test_double_escaped_maximum_metadata_is_not_under_reserved():
    args = {**ARGS, "files": [{**ARGS["files"][0], "path": "\ufffd" * 1300}], "argv": ["\ufffd" * 150] * 4}
    identity = ["\ufffd" * 80] * 4
    python.validate(args)
    assert len(python.encoded(identity)) <= 2048
    reservation = python.reserve_receipt([], args, identity)
    old = python.encoded({"version": 1, "runs": []}).decode() + " " * 48000
    assert len(reservation) < python.MAX_RECEIPTS
    assert len(python.encoded(reservation)) > len(python.encoded(old))
    assert json.loads(reservation) == {"version": 1, "runs": []}


@pytest.mark.parametrize("byte", [0, 9, 34, 92, 127, 128, 255])
def test_reservation_bounds_raw_and_outer_json_without_fabricated_receipts(byte):
    args = {**ARGS, "argv": ['literal\\"☃']}
    previous = [{"ok": False, "reason": "timeout", "exit_code": -9, "descendants_cleaned": True}]
    reservation = python.reserve_receipt(previous, args, IDENTITY)
    data = bytes([byte]) * python.OUTPUT_BYTES
    stream = {
        "text": data.decode("utf-8", errors="replace"),
        "base64": base64.b64encode(data).decode(),
        "sha256": python.digest(data),
        "observed_bytes": 6144,
        "truncated": True,
    }
    receipt = {
        "version": 1,
        "identity": IDENTITY,
        "entrypoint": args["entrypoint"],
        "files": args["files"],
        "argv": args["argv"],
        "timeout_seconds": 30,
        "elapsed_ms": 70000,
        "reason": "output_limit",
        "exit_code": -9,
        "descendants_cleaned": True,
        "network": "existing-agent-governed-proxy; no claim of live collection",
        "stdout": stream,
        "stderr": stream,
        "ok": False,
    }
    actual = python.append_receipt(previous, receipt)
    assert len(actual.encode()) <= len(reservation.encode()) <= python.MAX_RECEIPTS
    assert len(python.encoded(actual)) <= len(python.encoded(reservation))
    assert json.loads(reservation) == {"version": 1, "runs": previous}


@pytest.mark.parametrize("hard", [python.resource.RLIM_INFINITY, 16])
def test_process_limits_do_not_charge_other_sandboxes_sharing_the_uid(hard):
    with (
        patch.object(python.resource, "getrlimit", return_value=(hard, hard)),
        patch.object(python.resource, "setrlimit") as set_limit,
    ):
        python._limits()
    ceilings = {
        python.resource.RLIMIT_CPU: 30,
        python.resource.RLIMIT_AS: 512 * 1024 * 1024,
        python.resource.RLIMIT_FSIZE: 8 * 1024 * 1024,
        python.resource.RLIMIT_NOFILE: 64,
        python.resource.RLIMIT_CORE: 0,
    }
    assert {call.args[0] for call in set_limit.call_args_list} == set(ceilings)
    for call in set_limit.call_args_list:
        expected = (
            ceilings[call.args[0]] if hard == python.resource.RLIM_INFINITY else min(hard, ceilings[call.args[0]])
        )
        assert call.args[1] == (expected, expected)


def test_non_linux_fails_before_materialization():
    with patch.object(python.sys, "platform", "darwin"), pytest.raises(RuntimeError, match="Linux runtime"):
        python.supervise({})


def test_python_requires_explicit_native_file_and_network_authority():
    phase = {
        "name": "test",
        "objective": "Execute and retain real test outputs",
        "maxToolCalls": 8,
        "executionMode": "native",
        "capabilities": ["filesystem-read", "filesystem-write", "network", "python-execute"],
    }
    assert "python-execute" in wire.phase(phase)["capabilities"]
    for capability in ("filesystem-read", "filesystem-write", "network"):
        with pytest.raises(ValueError):
            wire.phase({**phase, "capabilities": [c for c in phase["capabilities"] if c != capability]})
    del phase["executionMode"]
    with pytest.raises(ValueError):
        wire.phase(phase)


class Observations:
    scope = "scope-1"

    def __init__(self, allow=True):
        self.allow = allow
        self.calls = []

    def authorize_tool(self, *args):
        self.calls.append(("authorize", args))
        return self.allow

    def report_tool(self, *args):
        self.calls.append(("report", args))
        return True

    def artifact(self):
        return None


def tools_at(root, allow=True):
    for name in ("inputs", "scratch", "exports"):
        (root / name).mkdir()
    phase = wire.phase(
        {
            "name": "test",
            "objective": "Execute and retain actual program output",
            "maxToolCalls": 8,
            "executionMode": "native",
            "capabilities": ["filesystem-read", "filesystem-write", "network", "python-execute"],
        }
    )
    m = Measurements("test-model", phase, lambda _: None, Observations(allow))
    tools = NativeTools(root, m)
    tools.python_identity = IDENTITY[:3]
    tools.owned_files[root / "scratch" / "app.py"] = SOURCE
    m.attempted = 1
    tools.start("call-1", "python_execute", ARGS)
    return tools


@pytest.mark.parametrize("exit_code", [0, 7])
def test_adapter_retains_actual_failure_or_success_and_consumes_admission(tmp_path, exit_code):
    tools = tools_at(tmp_path)
    receipt = {
        "version": 1,
        "ok": exit_code == 0,
        "reason": "exited",
        "exit_code": exit_code,
        "descendants_cleaned": True,
        "stdout": {"text": "actual output"},
    }
    with patch.object(python, "invoke", return_value=receipt) as invoke:
        assert json.loads(tools.invoke_python(ARGS)) == receipt
        invoke.assert_called_once()
        assert invoke.call_args.args[:3] == (ARGS, [SOURCE], IDENTITY)
        assert tools.m.successful == int(exit_code == 0)
        artifact = tools.execution_artifacts()[python.ARTIFACT]
        assert json.loads(artifact)["runs"] == [receipt]
        assert (tmp_path / "exports" / python.ARTIFACT).read_text() == artifact
        assert tools.m.observations.calls[-1][1][3]["kind"] == "python_execution"
        assert "error" in json.loads(tools.invoke_python(ARGS))
        assert invoke.call_count == 1
    with pytest.raises(ValueError):
        tools.checked_path("exports/" + python.ARTIFACT, write=True)


@pytest.mark.parametrize("denial", ["policy", "digest", "ambient"])
def test_adapter_never_launches_denied_or_unowned_bytes(tmp_path, denial):
    tools = tools_at(tmp_path, allow=denial != "policy")
    args = copy.deepcopy(ARGS)
    if denial == "digest":
        args["files"][0]["sha256"] = "sha256:" + "0" * 64
    if denial == "ambient":
        tools.owned_files.clear()
        (tmp_path / "scratch" / "app.py").write_text(SOURCE)
    with patch.object(python, "invoke") as invoke:
        assert "error" in json.loads(tools.invoke_python(args))
        invoke.assert_not_called()
    assert tools.m.successful == 0 and not tools.python_receipts


def test_adapter_executes_with_90000_bytes_of_exports_and_retains_failure(tmp_path):
    tools = tools_at(tmp_path)
    (tmp_path / "exports" / "report.md").write_text("x" * 45000)
    (tmp_path / "exports" / "research.md").write_text("y" * 45000)
    receipt = {"ok": False, "reason": "exited", "exit_code": 7, "descendants_cleaned": True}
    with patch.object(python, "invoke", return_value=receipt) as invoke:
        assert json.loads(tools.invoke_python(ARGS)) == receipt
        invoke.assert_called_once()
    assert json.loads(tools.execution_artifacts()[python.ARTIFACT])["runs"] == [receipt]
    assert tools.m.successful == 0
    assert (tmp_path / "exports" / "report.md").read_text() == "x" * 45000
    assert (tmp_path / "exports" / "research.md").read_text() == "y" * 45000
    wire.artifacts(
        {
            "report.md": "x" * 45000,
            "research.md": "y" * 45000,
            python.ARTIFACT: tools.execution_artifacts()[python.ARTIFACT],
        }
    )


def test_adapter_explains_receipt_capacity_and_accepts_shorter_export_retry(tmp_path):
    tools = tools_at(tmp_path)
    report = tmp_path / "exports" / "report.md"
    report.write_text("x" * 60000)
    (tmp_path / "exports" / "research.md").write_text("y" * 60000)
    receipt = {"ok": True, "reason": "exited", "exit_code": 0, "descendants_cleaned": True}
    with patch.object(python, "invoke", return_value=receipt) as invoke:
        error = json.loads(tools.invoke_python(ARGS))["error"]
        assert "Python has not started:" in error and "execution receipt" in error
        assert "Shorten existing exports and retry" in error
        invoke.assert_not_called()
        assert report.read_text() == "x" * 60000
        assert not tools.python_receipts and tools.m.successful == 0
        assert tools.m.observations.calls[-1][1][1] is False
        report.write_text("Short report; research inputs remain available without republishing.")
        tools.m.attempted += 1
        tools.start("call-2", "python_execute", ARGS)
        assert json.loads(tools.invoke_python(ARGS)) == receipt
        invoke.assert_called_once()
        assert tools.m.successful == 1
        assert json.loads(tools.execution_artifacts()[python.ARTIFACT])["runs"] == [receipt]


SANDBOX = pytest.mark.skipif(
    sys.platform != "linux" or os.getuid() != 1000 or os.environ.get("KARS_PYTHON_SANDBOX_TESTS") != "1",
    reason="Requires opt-in real Linux Task sandbox UID 1000; not host validation",
)


def run_source(source, **kwargs):
    args = {
        "entrypoint": "app.py",
        "files": [{"path": "inputs/app.py", "name": "app.py", "sha256": python.digest(source.encode())}],
        **kwargs,
    }
    reservation = python.reserve_receipt([], args, IDENTITY)
    result = python.invoke(args, [source], IDENTITY)
    actual = python.append_receipt([], result)
    assert len(actual.encode()) <= len(reservation.encode())
    assert len(python.encoded(actual)) <= len(python.encoded(reservation))
    return result


@SANDBOX
def test_linux_full_invalid_utf8_streams_fit_reserved_receipt():
    result = run_source("import os\nos.write(1, b'\\xff' * 2048)\nos.write(2, b'\\xff' * 2048)\n")
    assert result["ok"] and result["descendants_cleaned"]
    for name in ("stdout", "stderr"):
        assert result[name]["text"] == "\ufffd" * 2048
        assert base64.b64decode(result[name]["base64"]) == b"\xff" * 2048


@SANDBOX
def test_linux_real_stdout_stderr_status_environment_and_limits():
    result = run_source(
        "import os, resource, sys\nprint('actual')\nprint('failure', file=sys.stderr)\n"
        "assert os.environ['HTTPS_PROXY'] == 'http://127.0.0.1:8444'\n"
        "assert 'AZURE_OPENAI_API_KEY' not in os.environ\n"
        "assert resource.getrlimit(resource.RLIMIT_NOFILE)[1] <= 64\n"
        "sys.exit(7)\n"
    )
    assert result["exit_code"] == 7 and result["reason"] == "exited" and not result["ok"]
    assert base64.b64decode(result["stdout"]["base64"]) == b"actual\n"
    assert base64.b64decode(result["stderr"]["base64"]) == b"failure\n"
    assert result["descendants_cleaned"]


@SANDBOX
def test_linux_timeout_and_detached_descendant_cleanup():
    result = run_source(
        "import os, time\npid=os.fork()\n"
        "if pid == 0:\n os.setsid()\n print(os.getpid(), flush=True)\n time.sleep(90)\n"
        "else:\n time.sleep(90)\n",
        timeout_seconds=1,
    )
    assert result["reason"] == "timeout" and result["descendants_cleaned"], result["stderr"]["text"]
    with pytest.raises(ProcessLookupError):
        os.kill(int(result["stdout"]["text"].strip()), 0)


@SANDBOX
def test_linux_invocation_process_overflow_cleans_up():
    result = run_source(
        "import os, time\n"
        "for _ in range(65):\n"
        " pid = os.fork()\n"
        " if pid == 0:\n  time.sleep(20)\n  os._exit(0)\n"
        " print(pid, flush=True)\n"
        "time.sleep(20)\n",
        timeout_seconds=10,
    )
    assert result["reason"] == "process_limit" and not result["ok"], result["stderr"]["text"]
    assert result["descendants_cleaned"]
    for pid in result["stdout"]["text"].split():
        with pytest.raises(ProcessLookupError):
            os.kill(int(pid), 0)


@SANDBOX
def test_linux_output_overflow_is_explicit():
    result = run_source("print('x' * 100000)\n")
    assert result["reason"] == "output_limit" and result["stdout"]["truncated"] and not result["ok"]
    assert len(base64.b64decode(result["stdout"]["base64"])) == python.OUTPUT_BYTES


@SANDBOX
def test_linux_cancellation_returns_real_receipt():
    source = "import time\nprint('started', flush=True)\ntime.sleep(90)\n"
    args = {
        "entrypoint": "app.py",
        "files": [{"path": "inputs/app.py", "name": "app.py", "sha256": python.digest(source.encode())}],
    }
    started = time.monotonic()
    result = python.invoke(args, [source], IDENTITY, lambda: time.monotonic() - started > 0.5)
    assert result["reason"] == "cancelled" and not result["ok"] and result["descendants_cleaned"]
