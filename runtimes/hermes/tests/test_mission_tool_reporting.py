# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json

import httpx
import pytest

from kars_runtime_hermes.plugin.mission_native import FilesystemTools, Measurements, MeasuredTransport, ROUTER
from kars_runtime_hermes.plugin.mission_observations import ReturnedRouterResponses


@pytest.mark.parametrize(
    "code",
    [
        "token_accounting_unavailable",
        "inference_budget_unavailable",
        "inference_budget_authority",
        "inference_budget_attempt_state",
        "inference_budget_exhausted",
        "inference_budget_capacity",
    ],
)
@pytest.mark.parametrize("status", [400, 503])
@pytest.mark.parametrize("prior", [None, "finish=tool_calls, textChars=0, toolCalls=1, refusal=False"])
def test_inference_rejection_retains_known_code_without_response_content_or_retry(code, status, prior):
    calls = []

    def reject(request):
        calls.append(request)
        return httpx.Response(
            status, json={"error": {"code": code, "message": "private response", "type": "private type"}}
        )

    measurements = Measurements("model", None, lambda _: None)
    measurements.last_completion = prior
    boundary = MeasuredTransport(measurements, httpx.MockTransport(reject), set())
    with httpx.Client(transport=boundary) as client:
        for _ in range(2):
            with pytest.raises(RuntimeError):
                client.post(ROUTER + "/chat/completions", json={"model": "model"})
    expected = f"Inference request rejected (HTTP {status}); reported code={code}"
    if code == "token_accounting_unavailable":
        expected += "; reported accounting stage=unclassified"
    assert measurements.failure == expected + ("; prior completion " + prior if prior else "")
    assert measurements.usage is None and len(calls) == 1
    assert "private" not in measurements.failure


@pytest.mark.parametrize(
    "body",
    [
        b"private plaintext",
        b"[]",
        b"null",
        b'{"error":"private error"}',
        b'{"error":{"code":"private-code","message":"private"}}',
        b'{"error":{"code":["inference_budget_unavailable"]}}',
        b'{"error":{"code":{"private":"value"}}}',
        b"\xff",
    ],
)
def test_inference_rejection_does_not_echo_unknown_or_malformed_errors(body):
    measurements = Measurements("model", None, lambda _: None)
    boundary = MeasuredTransport(measurements, httpx.MockTransport(lambda _: httpx.Response(503, content=body)), set())
    with httpx.Client(transport=boundary) as client:
        with pytest.raises(RuntimeError):
            client.post(ROUTER + "/chat/completions", json={"model": "model"})
    assert measurements.failure == "Inference request rejected (HTTP 503); reported code=unclassified"
    assert measurements.usage is None


def reporter(handler):
    value = ReturnedRouterResponses(httpx.MockTransport(handler))
    value.capture(httpx.Headers({"x-kars-telemetry-scope": "scope-1", "x-kars-telemetry-round": "1"}))
    return value


@pytest.mark.parametrize("operation", ["read", "write", "bundle", "skill", "missing", "immutable"])
def test_filesystem_reports_only_correlated_bounded_metadata(tmp_path, input_artifact, operation):
    content = "Confidential evidence\r\n✓"
    inputs = [input_artifact("facts.md", content)] if operation in ("bundle", "immutable") else []
    (tmp_path / "inputs").mkdir()
    (tmp_path / "exports").mkdir()
    (tmp_path / "inputs/facts.md").write_bytes(content.encode())
    requests = []

    authorized = []

    def handle(request):
        if request.url.path == "/agt/evaluate":
            decision = json.loads(request.content)
            assert decision == {
                "action": "tool:kars_mission_" + mode,
                "context": {"scope_id": "scope-1", "tool_call_id": "call-1"},
            }
            authorized.append(decision)
            return httpx.Response(200, json={"allowed": True})
        assert len(authorized) == 1
        assert str(request.url) == "http://127.0.0.1:8443/telemetry/tool"
        assert request.method == "POST" and request.extensions["timeout"]["read"] == 2
        requests.append(json.loads(request.content))
        return httpx.Response(200, json={"recorded": True, "source": "harness-reported", "enforcement_changed": False})

    class Skills:
        def read(self, path):
            assert path == "skills/review/SKILL.md"
            return content

    measurements = Measurements("model", None, lambda _: None, reporter(handle))
    tools = FilesystemTools(tmp_path, measurements, Skills(), inputs)
    mode = "write" if operation in ("write", "immutable") else "read"
    paths = {
        "read": "inputs/facts.md",
        "write": "exports/report.md",
        "bundle": "inputs",
        "skill": "skills/review/SKILL.md",
        "missing": "inputs/missing.md",
        "immutable": "inputs/facts.md",
    }
    arguments = {"path": paths[operation], **({"content": content} if mode == "write" else {})}
    tools.start("call-1", "kars_mission_" + mode, arguments)
    result = json.loads(tools.execute(mode, arguments))
    assert len(requests) == 1
    report = requests[0]
    assert report["scope_id"] == "scope-1" and report["call_id"] == "call-1"
    assert type(report["latency_ms"]) is int and 0 <= report["latency_ms"] <= 3_600_000
    assert "Confidential" not in json.dumps(report) and "skills/" not in json.dumps(report)
    if operation in ("missing", "immutable"):
        assert "error" in result and measurements.successful == 0
        assert report["ok"] is False
        assert report["result_summary"] == {"kind": "failure", "reason": "filesystem_operation_failed"}
    elif operation == "write":
        assert measurements.successful == 1 and report["ok"] is True
        assert report["result_summary"] == {
            "kind": "file_write",
            "bytes": len(content.encode()),
            "artifact_name": "report.md",
        }
        assert (tmp_path / "exports/report.md").read_bytes() == content.encode()
    else:
        assert measurements.successful == 1 and report["ok"] is True
        size = len(result["content"].encode())
        assert report["result_summary"] == {
            "kind": "file_read",
            "bytes": size,
            "returned_bytes": size,
            "truncated": False,
        }
        if operation != "bundle":
            assert result["content"] == content
        else:
            assert json.loads(result["content"]) == inputs and not tools.unread
    # A handler without a new native start callback cannot reuse the last call ID.
    tools.execute(mode, arguments)
    assert len(requests) == 1


@pytest.mark.parametrize(
    "failure", ["status", "timeout", "oversize", "malformed", "unrecorded", "wrong-source", "enforcement"]
)
def test_reporting_failure_does_not_change_successful_execution(tmp_path, failure):
    (tmp_path / "exports").mkdir()

    def handle(request):
        if request.url.path == "/agt/evaluate":
            return httpx.Response(200, json={"allowed": True})
        if failure == "timeout":
            raise httpx.ReadTimeout("recorder unavailable")
        if failure == "status":
            return httpx.Response(503)
        if failure in ("oversize", "malformed"):
            return httpx.Response(200, content=b"x" * (4097 if failure == "oversize" else 1))
        receipt = {"recorded": True, "source": "harness-reported", "enforcement_changed": False}
        receipt.update(
            {
                "unrecorded": {"recorded": False},
                "wrong-source": {"source": "model-proposed"},
                "enforcement": {"enforcement_changed": True},
            }[failure]
        )
        return httpx.Response(200, json=receipt)

    measurements = Measurements("model", None, lambda _: None, reporter(handle))
    tools = FilesystemTools(tmp_path, measurements)
    tools.start("call-1", "kars_mission_write", {})
    assert json.loads(tools.execute("write", {"path": "exports/report.md", "content": "Actual export"})) == {
        "written": "exports/report.md"
    }
    assert measurements.successful == 1
    assert (tmp_path / "exports/report.md").read_text() == "Actual export"


@pytest.mark.parametrize("failure", ["deny", "status", "timeout", "oversize", "malformed", "missing", "truthy"])
@pytest.mark.parametrize("mode", ["read", "write"])
def test_authorization_failure_prevents_filesystem_access(tmp_path, failure, mode):
    (tmp_path / "exports").mkdir()
    target = tmp_path / "exports/report.md"
    target.write_text("Original bytes")
    paths = []

    def handle(request):
        paths.append(request.url.path)
        assert request.url.path == "/agt/evaluate"
        if failure == "timeout":
            raise httpx.ReadTimeout("unavailable")
        if failure == "status":
            return httpx.Response(503)
        if failure in ("oversize", "malformed"):
            return httpx.Response(200, content=b"x" * (65537 if failure == "oversize" else 1))
        return httpx.Response(
            200, json={"deny": {"allowed": False}, "missing": {}, "truthy": {"allowed": "true"}}[failure]
        )

    measurements = Measurements("model", None, lambda _: None, reporter(handle))
    tools = FilesystemTools(tmp_path, measurements)
    args = {"path": "exports/report.md", **({"content": "Replacement"} if mode == "write" else {})}
    tools.start("call-1", "kars_mission_" + mode, args)
    assert json.loads(tools.execute(mode, args)) == {"error": "Filesystem tool authorization refused"}
    assert target.read_text() == "Original bytes" and measurements.successful == 0
    assert not tools.written and paths == ["/agt/evaluate"]


@pytest.mark.parametrize("correlation", ["changed", "invalid-call", "wrong-tool"])
def test_invalid_authorization_correlation_never_contacts_router(correlation):
    observations = reporter(lambda _: pytest.fail("Uncorrelated authorization"))
    if correlation == "changed":
        observations.capture(httpx.Headers({"x-kars-telemetry-scope": "scope-2", "x-kars-telemetry-round": "2"}))
    assert (
        observations.authorize_tool(
            "bad call" if correlation == "invalid-call" else "call-1",
            "shell" if correlation == "wrong-tool" else "kars_mission_read",
        )
        is False
    )


@pytest.mark.parametrize("correlation", ["absent", "changed", "invalid-call"])
def test_no_tool_post_without_valid_returned_scope(correlation):
    observations = ReturnedRouterResponses(httpx.MockTransport(lambda _: pytest.fail("Uncorrelated report")))
    if correlation != "absent":
        observations.capture(httpx.Headers({"x-kars-telemetry-scope": "scope-1", "x-kars-telemetry-round": "1"}))
    if correlation == "changed":
        observations.capture(httpx.Headers({"x-kars-telemetry-scope": "scope-2", "x-kars-telemetry-round": "2"}))
    assert observations.report_tool("bad call" if correlation == "invalid-call" else "call-1", True, 1, None) is False
