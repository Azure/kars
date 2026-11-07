# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Local authority/measurement tests; these do not claim native SDK qualification."""

import json
from types import SimpleNamespace

import httpx
import pytest

from kars_runtime_hermes.plugin import mission_native as native
from kars_runtime_hermes.plugin import mission_protocol as wire
from kars_runtime_hermes.plugin.mission_native_tools import NativeTools, native_write_schema
from kars_runtime_hermes.plugin.mission_observations import ARTIFACT, ReturnedRouterResponses


@pytest.mark.parametrize("version", ["0.16.1", "0.21.4", "0.21.5rc1", "0.21.6", "1.0.0"])
def test_unqualified_runtime_versions_are_rejected(monkeypatch, version):
    monkeypatch.setattr(native.importlib.metadata, "version", lambda _: version)
    with pytest.raises(ValueError, match="Hermes 0.16.0 or 0.21.5 is required"):
        native.require_supported_runtime()


def test_missing_runtime_metadata_is_rejected(monkeypatch):
    def missing(name):
        raise native.importlib.metadata.PackageNotFoundError(name)

    monkeypatch.setattr(native.importlib.metadata, "version", missing)
    with pytest.raises(native.importlib.metadata.PackageNotFoundError):
        native.require_supported_runtime()


def phase(calls=()):
    return wire.phase(
        {
            "name": "research",
            "objective": "Research and produce a substantive report",
            "executionMode": "native",
            "capabilities": ["network", "filesystem-read", "filesystem-write"],
            "requiredToolCalls": list(calls),
            "maxToolCalls": 8,
        }
    )


def measured(reviewed, provider):
    observations = ReturnedRouterResponses(httpx.MockTransport(provider))
    return native.Measurements("test-model", reviewed, lambda _: None, observations)


@pytest.mark.parametrize(
    "status,truncated,body",
    [
        (403, False, "Forbidden"),
        (302, False, ""),
        (200, True, "Partial"),
        (200, False, ""),
        (200, False, "x" * (2 * 1024 * 1024)),
        (200, True, "x" * (2 * 1024 * 1024) + "... [truncated at 2097152 bytes]"),
    ],
)
def test_native_fetch_returns_status_and_large_body_as_data(tmp_path, status, truncated, body):
    requests = []
    envelope = {"status": status, "headers": {"content-type": "text/plain"}, "body": body, "truncated": truncated}

    def provider(req):
        requests.append(req.url.path)
        return httpx.Response(200, json={"allowed": True} if req.url.path == "/agt/evaluate" else envelope)

    m = measured(phase(), provider)
    tools = NativeTools(tmp_path, m)
    tools.unread = {"inputs/not-forced.md"}
    args = {"method": "GET", "url": "https://example.com/agent-selected"}
    m.attempted = 1
    tools.start("call-1", "http_fetch", args)
    assert json.loads(tools.fetch(args)) == envelope
    assert requests == ["/agt/evaluate", "/egress/fetch"]
    assert m.failure is None
    assert m.successful == int(200 <= status < 300)
    assert not m.completed_fetches


@pytest.mark.parametrize("reserve", [False, True])
def test_native_invalid_fetch_reports_only_reserved_observed_calls(tmp_path, reserve):
    reports = []

    def provider(req):
        assert req.url.path == "/telemetry/tool"
        reports.append(json.loads(req.content))
        return httpx.Response(200, json={"recorded": True, "source": "harness-reported", "enforcement_changed": False})

    m = measured(phase(), provider)
    m.observations.scope = "scope-1"
    tools = NativeTools(tmp_path, m)
    args = {"method": "POST", "url": "https://example.net"}
    if reserve:
        m.attempted = 1
        tools.start("invalid-fetch", "http_fetch", args)
    assert "error" in json.loads(tools.fetch(args))
    assert m.successful == 0
    assert len(reports) == int(reserve)
    if reserve:
        assert reports[0]["call_id"] == "invalid-fetch" and reports[0]["ok"] is False
        assert reports[0]["result_summary"] == {"kind": "failure", "reason": "tool_execution_failed"}
    tools.fetch(args)
    assert len(reports) == int(reserve)


def test_native_router_denial_preserves_http_status_without_halting(tmp_path):
    m = measured(
        phase(),
        lambda req: (
            httpx.Response(200, json={"allowed": True})
            if req.url.path == "/agt/evaluate"
            else httpx.Response(403, json={"error": "Denied"})
        ),
    )
    tools = NativeTools(tmp_path, m)
    args = {"method": "GET", "url": "https://example.com/denied"}
    m.attempted = 1
    tools.start("call-denied", "http_fetch", args)
    assert json.loads(tools.fetch(args)) == {"error": "Denied", "http_status": 403}
    assert m.failure is None and m.successful == 0
    assert not m.completed_fetches


def test_native_github_fetch_uses_public_get_and_preserves_keyless_observation(tmp_path):
    args = {"method": "GET", "url": "https://api.github.com/repos/owner/repo/issues"}
    envelope = {"status": 200, "headers": {"cache-control": "no-store"}, "body": '[{"number":42}]', "truncated": False}
    requests = []

    def provider(req):
        requests.append(req)
        return httpx.Response(200, json={"allowed": True} if req.url.path == "/agt/evaluate" else envelope)

    m = measured(phase([{"name": "http_fetch", "arguments": args}]), provider)
    tools = NativeTools(tmp_path, m)
    m.attempted = 1
    tools.start("github-get", "http_fetch", args)
    assert json.loads(tools.fetch(args)) == envelope
    sent = next(req for req in requests if req.url.path == "/egress/fetch")
    assert json.loads(sent.content) == {**args, "headers": {}, "body": ""}
    assert not m.pending_fetches() and m.successful == 1


def test_native_required_fetch_remains_an_obligation_not_a_url_allowlist(tmp_path):
    required = {"method": "GET", "url": "https://example.com/required"}
    m = measured(
        phase([{"name": "http_fetch", "arguments": required}]),
        lambda req: httpx.Response(
            200,
            json={"allowed": True}
            if req.url.path == "/agt/evaluate"
            else {"status": 200, "headers": {}, "body": "Evidence", "truncated": False},
        ),
    )
    tools = NativeTools(tmp_path, m)
    for index, args in enumerate(({**required, "url": "https://example.com/chosen"}, required)):
        m.attempted += 1
        tools.start(f"call-{index}", "http_fetch", args)
        assert json.loads(tools.fetch(args))["body"] == "Evidence"
        assert len(m.pending_fetches()) == 1 - index


@pytest.mark.parametrize("decision", ["approved", "denied", "cancelled", "expired"])
def test_native_clarification_continues_with_measured_scope(tmp_path, decision):
    requests = []

    def provider(req):
        requests.append(req)
        if req.url.path == "/agt/evaluate":
            return httpx.Response(200, json={"allowed": True})
        if req.url.path == "/telemetry/tool":
            return httpx.Response(
                200, json={"recorded": True, "source": "harness-reported", "enforcement_changed": False}
            )
        entry = {"scope_id": "scope-1", "request_id": "request-1", "kind": "clarification", "status": "pending"}
        if req.url.path == "/v1/access-request":
            return httpx.Response(202, json={"status": "queued", "request": entry})
        if req.url.path.endswith("/wait"):
            return httpx.Response(
                200, json={**entry, "status": decision, "answer": "Use the report" if decision == "approved" else None}
            )
        return httpx.Response(200, json={"status": 200, "headers": {}, "body": "Evidence", "truncated": False})

    m = measured(phase(), provider)
    m.observations.scope = "scope-1"
    tools = NativeTools(tmp_path, m)
    args = {"operation": "request", "kind": "clarification", "target": "source", "reason": "Which source?"}
    m.attempted = 1
    tools.start("question-1", "kars_access_request", args)
    result = json.loads(tools.invoke_access(args))
    assert result["request"]["status"] == decision and result["enforcement_changed"] is False
    if decision == "approved":
        assert result["request"]["answer"] == "Use the report"
    assert m.successful == 1 and m.failure is None
    m.attempted += 1
    fetch_args = {"method": "GET", "url": "https://example.com/report"}
    tools.start("continue-1", "http_fetch", fetch_args)
    assert json.loads(tools.fetch(fetch_args))["body"] == "Evidence"
    assert m.successful == 2
    evaluation = json.loads(requests[0].content)
    assert evaluation["context"] == {
        "scope_id": "scope-1",
        "tool_call_id": "question-1",
        "tool": "kars_access_request",
        "arguments": args,
    }
    wait = next(req for req in requests if req.url.path.endswith("/wait"))
    assert wait.url.params["scope_id"] == "scope-1" and wait.extensions["timeout"]["read"] > 300
    reports = [json.loads(req.content) for req in requests if req.url.path == "/telemetry/tool"]
    assert [(report["call_id"], report["ok"]) for report in reports] == [("question-1", True), ("continue-1", True)]


def test_native_access_rejects_unobserved_scope_and_stale_resume(tmp_path):
    m = measured(phase(), lambda req: httpx.Response(200, json={"allowed": True}))
    tools = NativeTools(tmp_path, m)
    args = {"operation": "wait", "scope_id": "old", "request_id": "request-1"}
    m.attempted = 1
    tools.start("call-1", "kars_access_request", args)
    assert "observed assignment scope" in json.loads(tools.invoke_access(args))["error"]
    m.observations.scope = "new"
    tools.start("call-2", "kars_access_request", args)
    assert "stale_scope" in json.loads(tools.invoke_access(args))["error"]
    assert not m.successful


def test_native_fetch_wait_preserves_scope_denial_and_required_get(tmp_path):
    required = {"method": "GET", "url": "https://example.com/required"}
    requests = []
    entry = {"scope_id": "scope-1", "request_id": "request-1", "status": "approved"}
    count = 0

    def provider(req):
        nonlocal count
        requests.append(req)
        if req.url.path == "/agt/evaluate":
            return httpx.Response(200, json={"allowed": True})
        if req.url.path == "/telemetry/tool":
            return httpx.Response(
                200, json={"recorded": True, "source": "harness-reported", "enforcement_changed": False}
            )
        count += 1
        if count == 1:
            return httpx.Response(
                403, json={"error": "policy_not_installed", "request": entry, "enforcement_changed": False}
            )
        return httpx.Response(200, json={"status": 200, "headers": {}, "body": "Evidence", "truncated": False})

    m = measured(phase([{"name": "http_fetch", "arguments": required}]), provider)
    m.observations.scope = "scope-1"
    tools = NativeTools(tmp_path, m)
    args = {**required, "wait_for_approval_ms": 300000}
    m.attempted = 1
    tools.start("call-1", "http_fetch", args)
    assert json.loads(tools.fetch(args)) == {
        "error": "policy_not_installed",
        "http_status": 403,
        "request": entry,
        "enforcement_changed": False,
    }
    assert not m.successful and m.pending_fetches()
    m.attempted += 1
    tools.start("call-2", "http_fetch", args)
    assert json.loads(tools.fetch(args))["body"] == "Evidence"
    assert m.successful == 1 and not m.pending_fetches()
    fetches = [req for req in requests if req.url.path == "/egress/fetch"]
    for index, req in enumerate(fetches, 1):
        body = json.loads(req.content)
        assert body["scope_id"] == "scope-1" and body["wait_for_approval_ms"] == 300000
        assert body["telemetry"] == {"scope_id": "scope-1", "call_id": f"call-{index}"}
        assert req.extensions["timeout"]["read"] > 300


@pytest.fixture
def native_files(tmp_path, input_artifact):
    for directory in ("inputs", "exports", "scratch", "skills", "home"):
        (tmp_path / directory).mkdir()
    item = input_artifact("facts.md", "Immutable facts")
    (tmp_path / "inputs/facts.md").write_text(item["content"])
    m = measured(phase(), lambda _: httpx.Response(200, json={"allowed": True}))
    tools = NativeTools(tmp_path, m, input_artifacts=[item])
    schemas = {
        "read_file": {"properties": {"path": {}, "offset": {}, "limit": {}}, "required": ["path"]},
        "write_file": {"properties": {"path": {}, "content": {}}, "required": ["path", "content"]},
        "search_files": {"properties": {"path": {}, "pattern": {}}, "required": ["pattern"]},
    }
    schemas["write_file"] = native_write_schema({"parameters": schemas["write_file"]})["parameters"]
    tools.registry = SimpleNamespace(get_schema=lambda name: {"parameters": schemas[name]})
    return tools, m


def invoke(tools, m, name, args, handler):
    m.attempted += 1
    tools.start(f"call-{m.attempted}", name, args)
    return json.loads(tools.invoke(name, handler, args, task_id="assignment"))


def persist_copy(args, **kwargs):
    from pathlib import Path

    Path(args["path"]).write_bytes(args["content"].encode("utf-8"))
    return json.dumps({"success": True})


@pytest.mark.parametrize("source", ["input", "owned"])
@pytest.mark.parametrize("nullable", [False, True])
def test_native_copy_preserves_exact_bytes_and_original_authorization(native_files, input_artifact, source, nullable):
    tools, m = native_files
    content = "\ufeff# café\r\nprint('雪')\r\n\u0000end"
    if source == "input":
        item = input_artifact("app.py", content)
        tools.inputs = [item]
        (tools.root / "inputs/app.py").write_bytes(content.encode())
        source_path = "inputs/app.py"
    else:
        source_path = "scratch/app.py"
        args = {"path": source_path, "content": content}
        if nullable:
            args.update(source_path=None, sha256=None)
        assert "error" not in invoke(tools, m, "write_file", args, persist_copy)
        # Copy the retained successful write, never ambient replacement bytes.
        (tools.root / source_path).write_bytes(b"external mutation")
    args = {"path": "exports/app.py", "source_path": source_path, "sha256": wire.text_hash(content)}
    if nullable:
        args["content"] = None
    requests = []
    m.observations.transport = httpx.MockTransport(
        lambda req: requests.append(json.loads(req.content)) or httpx.Response(200, json={"allowed": True})
    )
    result = invoke(tools, m, "write_file", args, persist_copy)
    assert result == {"success": True, "sha256": args["sha256"]}
    assert (tools.root / args["path"]).read_bytes() == content.encode()
    assert tools.owned_files[tools.root / args["path"]] == content
    assert "app.py" in tools.written
    assert requests[0]["context"]["arguments"] == args
    tools.validate_exports(native.read_exports(tools.root / "exports"))


@pytest.mark.parametrize(
    "fault",
    [
        "digest",
        "ambient",
        "mixed",
        "missing-digest",
        "missing-source",
        "host",
        "skills",
        "symlink",
        "reserved-python",
        "reserved-router",
        "read-authority",
        "write-authority",
        "policy",
        "append",
    ],
)
def test_native_copy_refuses_unowned_or_unauthorized_sources_before_handler(native_files, fault):
    tools, m = native_files
    args = {"path": "exports/report.md", "source_path": "inputs/facts.md", "sha256": wire.text_hash("Immutable facts")}
    if fault == "digest":
        args["sha256"] = "sha256:" + "0" * 64
    elif fault == "ambient":
        args["source_path"] = "scratch/ambient.py"
        (tools.root / args["source_path"]).write_bytes(b"Immutable facts")
    elif fault == "mixed":
        args["content"] = "Immutable facts"
    elif fault == "missing-digest":
        del args["sha256"]
    elif fault == "missing-source":
        del args["source_path"]
    elif fault == "host":
        args["source_path"] = "/etc/passwd"
    elif fault == "skills":
        args["source_path"] = "skills/example.py"
        (tools.root / args["source_path"]).write_bytes(b"Immutable facts")
    elif fault == "symlink":
        args["source_path"] = "scratch/link.py"
        (tools.root / args["source_path"]).symlink_to(tools.root / "inputs/facts.md")
    elif fault.startswith("reserved-"):
        from kars_runtime_hermes.plugin.native_python import ARTIFACT as PYTHON_ARTIFACT

        args["path"] = "exports/" + (ARTIFACT if fault == "reserved-router" else PYTHON_ARTIFACT)
    elif fault.endswith("-authority"):
        m.phase["capabilities"].remove("filesystem-" + fault.split("-")[0])
    elif fault == "policy":
        m.observations.transport = httpx.MockTransport(lambda _: httpx.Response(200, json={"allowed": False}))
    elif fault == "append":
        args["append"] = True
    called = []
    result = invoke(tools, m, "write_file", args, lambda *a, **kw: called.append(a))
    assert "error" in result and not called and m.successful == 0
    assert not (tools.root / "exports/report.md").exists()


@pytest.mark.parametrize("limit", ["count", "serialized", "file"])
def test_native_copy_obeys_existing_limits_before_writing(native_files, limit):
    tools, m = native_files
    content = "source"
    if limit == "count":
        for i in range(16):
            (tools.root / "exports" / f"{i}.txt").write_bytes(b"existing")
    elif limit == "serialized":
        (tools.root / "exports/first.txt").write_bytes(b"a" * 65000)
        content = '"' * 34000
    else:
        content = "a" * 65537
    tools.owned_files[tools.root / "scratch/source.py"] = content
    args = {"path": "exports/report.md", "source_path": "scratch/source.py", "sha256": wire.text_hash(content)}
    called = []
    assert "error" in invoke(tools, m, "write_file", args, lambda *a, **kw: called.append(a))
    assert not called and not (tools.root / "exports/report.md").exists()
    with pytest.raises(ValueError, match="Unresolved"):
        tools.validate_exports({})


def test_native_copy_failure_requires_retry_and_final_bytes_are_verified(native_files):
    tools, m = native_files
    args = {"path": "exports/report.md", "source_path": "inputs/facts.md", "sha256": wire.text_hash("Immutable facts")}
    assert "error" not in invoke(tools, m, "write_file", args, persist_copy)
    assert "error" in invoke(tools, m, "write_file", {**args, "sha256": "sha256:" + "0" * 64}, persist_copy)
    assert "report.md" not in tools.written
    with pytest.raises(ValueError, match="Unresolved"):
        tools.validate_exports(native.read_exports(tools.root / "exports"))
    assert "error" not in invoke(tools, m, "write_file", args, persist_copy)
    tools.validate_exports(native.read_exports(tools.root / "exports"))
    (tools.root / "exports/report.md").write_bytes(b"changed after write")
    with pytest.raises(ValueError, match="changed before artifact collection"):
        tools.validate_exports(native.read_exports(tools.root / "exports"))


def test_native_copy_does_not_credit_handler_byte_changes_or_reuse_admission(native_files):
    tools, m = native_files
    args = {"path": "exports/report.md", "source_path": "inputs/facts.md", "sha256": wire.text_hash("Immutable facts")}
    result = invoke(tools, m, "write_file", args, lambda args, **kw: persist_copy({**args, "content": "changed"}))
    assert "did not persist requested bytes" in result["error"]
    assert not tools.owned_files and not tools.written
    assert "error" in json.loads(tools.invoke("write_file", persist_copy, args))
    assert m.successful == 0


def test_native_copy_schema_keeps_content_and_does_not_mutate_native_registration():
    original = {
        "description": "Native writer",
        "parameters": {"properties": {"path": {}, "content": {}}, "required": ["path", "content"]},
    }
    schema = native_write_schema(original)
    assert original["parameters"]["required"] == ["path", "content"]
    assert "source_path" not in original["parameters"]["properties"]
    assert schema["parameters"]["required"] == ["path"]
    assert "oneOf" not in schema["parameters"]
    for key in ("content", "source_path", "sha256"):
        assert schema["parameters"]["properties"][key]["type"] == ["string", "null"]
    assert "without model transcription" in schema["description"]


def test_native_guard_calls_handler_without_forcing_read_first(native_files):
    tools, m = native_files
    seen = []

    def write(args, **kwargs):
        seen.append((args, kwargs))
        from pathlib import Path

        Path(args["path"]).write_text(args["content"])
        return json.dumps({"success": True})

    result = invoke(tools, m, "write_file", {"path": "exports/report.md", "content": "Actual findings"}, write)
    assert result == {"success": True, "sha256": wire.text_hash("Actual findings")}
    assert seen[0][1] == {"task_id": "assignment"}
    assert (tools.root / "exports/report.md").read_text() == "Actual findings"
    assert tools.unread == {"inputs/facts.md"}
    assert m.successful == 1
    tools.validate_inputs()


@pytest.mark.parametrize(
    "path",
    [
        "../escape",
        "/etc/passwd",
        "home/config.yaml",
        "inputs/facts.md",
        "skills/changed.md",
        f"exports/{ARTIFACT}",
        "exports/nested/report.md",
    ],
)
def test_native_guard_refuses_unauthorized_write_before_handler(native_files, path):
    tools, m = native_files
    called = []
    result = invoke(tools, m, "write_file", {"path": path, "content": "Bad"}, lambda *a, **kw: called.append(a))
    assert "error" in result
    assert not called and not m.successful
    tools.validate_inputs()


def test_native_guard_rejects_symlink_traversal_and_search(native_files):
    tools, m = native_files
    (tools.root / "scratch/link").symlink_to(tools.root / "home", target_is_directory=True)
    for name, args in (
        ("write_file", {"path": "scratch/link/config.yaml", "content": "bad"}),
        ("search_files", {"path": "scratch", "pattern": "*"}),
    ):
        result = invoke(tools, m, name, args, lambda *a, **kw: pytest.fail("Unsafe dispatch"))
        assert "symlink" in result["error"]


def test_native_guard_does_not_credit_handler_failure_or_fabricated_write(native_files):
    tools, m = native_files
    args = {"path": "exports/report.md", "content": "Not written"}
    assert "error" in invoke(tools, m, "write_file", args, lambda *a, **kw: '{"error":"Native refusal"}')
    assert "error" in invoke(tools, m, "write_file", args, lambda *a, **kw: '{"success":true}')
    assert m.successful == 0


def test_native_guard_rejects_extra_handler_bypass_arguments(native_files):
    tools, m = native_files
    args = {"path": "exports/report.md", "content": "Bad", "cross_profile": True}
    assert "error" in invoke(tools, m, "write_file", args, lambda *a, **kw: pytest.fail("Bypass dispatched"))


def test_native_guard_router_refusal_blocks_native_handler(native_files):
    tools, m = native_files
    m.observations.transport = httpx.MockTransport(lambda _: httpx.Response(200, json={"allowed": False}))
    assert (
        "authorization refused"
        in invoke(tools, m, "read_file", {"path": "inputs/facts.md"}, lambda *a, **kw: pytest.fail("Denied dispatch"))[
            "error"
        ]
    )


def test_native_guard_reads_issued_spill_with_real_page_arguments(native_files):
    tools, m = native_files
    path = tools.spill.persist("original large output" * 10000, "native-call")
    seen = []
    try:
        result = invoke(
            tools,
            m,
            "read_file",
            {"path": path, "offset": 40, "limit": 8},
            lambda args, **kw: seen.append(args) or '{"content":"native page"}',
        )
        assert result == {"content": "native page"}
        assert seen == [{"path": path, "offset": 40, "limit": 8}]
        assert m.successful == 1
        assert not tools.written
    finally:
        tools.spill.close()


@pytest.mark.parametrize("operation", ["write", "search", "unissued", "home", "large_scratch", "symlink", "page"])
def test_native_spill_access_does_not_expand_file_authority(native_files, operation):
    tools, m = native_files
    path = tools.spill.persist("large evidence" * 10000, "native-call")
    name, args = "read_file", {"path": path}
    if operation == "write":
        name, args = "write_file", {"path": path, "content": "replace evidence"}
    elif operation == "search":
        name, args = "search_files", {"path": "tool-results", "pattern": "*"}
    elif operation == "unissued":
        (tools.root / "tool-results/forged.txt").write_text("Not issued by persistence")
        args = {"path": "tool-results/forged.txt"}
    elif operation == "home":
        (tools.root / "home/secret").write_text("Never exposed")
        args = {"path": "home/secret"}
    elif operation == "large_scratch":
        (tools.root / "scratch/large.txt").write_text("x" * 65537)
        args = {"path": "scratch/large.txt"}
    elif operation == "symlink":
        (tools.root / "tool-results").rename(tools.root / "original-spills")
        (tools.root / "tool-results").symlink_to(tools.root / "original-spills", target_is_directory=True)
    else:
        args["limit"] = 33
    try:
        result = invoke(tools, m, name, args, lambda *a, **kw: pytest.fail("Unsafe spill dispatch"))
        assert "error" in result and m.successful == 0
    finally:
        tools.spill.close()


def test_native_guard_validates_immutable_inputs_even_without_reads(native_files):
    tools, _ = native_files
    (tools.root / "inputs/facts.md").write_text("Changed outside native tools")
    with pytest.raises(ValueError, match="Immutable input"):
        tools.validate_inputs()


@pytest.mark.parametrize("suffix", [".pdf", ".epub", ".docm", ".ipynb", ".sqlite"])
def test_native_guard_blocks_auxiliary_document_handlers(native_files, suffix):
    tools, m = native_files
    path = "scratch/source" + suffix
    (tools.root / path).write_text("Valid UTF-8 does not authorize document extraction")
    result = invoke(tools, m, "read_file", {"path": path}, lambda *a, **kw: pytest.fail("Auxiliary dispatch"))
    assert "auxiliary document extraction" in result["error"]
    assert m.successful == 0


def test_native_truncated_fetch_does_not_complete_required_obligation(tmp_path):
    args = {"method": "GET", "url": "https://example.com/required"}
    m = measured(
        phase([{"name": "http_fetch", "arguments": args}]),
        lambda req: httpx.Response(
            200,
            json={"allowed": True}
            if req.url.path == "/agt/evaluate"
            else {"status": 200, "headers": {}, "body": "Partial evidence", "truncated": True},
        ),
    )
    tools = NativeTools(tmp_path, m)
    m.attempted = 1
    tools.start("call-1", "http_fetch", args)
    assert json.loads(tools.fetch(args))["truncated"] is True
    assert len(m.pending_fetches()) == 1
    assert m.failure is None


def test_native_large_returned_content_keeps_measured_inference():
    requests = []

    def provider(request):
        requests.append(request)
        return httpx.Response(
            200,
            json={
                "choices": [
                    {"message": {"role": "assistant", "content": "Read the returned page"}, "finish_reason": "stop"}
                ],
                "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110},
            },
        )

    m = native.Measurements("test-model", phase(), lambda _: None)
    with httpx.Client(transport=native.MeasuredTransport(m, httpx.MockTransport(provider), set())) as client:
        response = client.post(
            native.ROUTER + "/chat/completions",
            json={
                "model": "test-model",
                "messages": [{"role": "tool", "content": "x" * (2 * 1024 * 1024)}],
            },
        )
    assert response.status_code == 200
    assert len(requests) == m.rounds == 1
    assert m.usage == {"promptTokens": 100, "completionTokens": 10, "totalTokens": 110}


@pytest.mark.parametrize(
    "message,stage",
    list(native.CALENDAR_FAILURE_STAGES.items())
    + [
        ("private-secret", "unclassified"),
        (
            "Calendar token accounting unavailable at /v1/calendar/commit; inference remains blocked private-secret",
            "unclassified",
        ),
        ({"private-secret": True}, "unclassified"),
        (None, "unclassified"),
    ],
)
def test_calendar_failure_retains_only_exact_fixed_stage(message, stage):
    requests = []

    def provider(request):
        requests.append(request)
        return httpx.Response(
            503,
            json={
                "error": {
                    "code": "token_accounting_unavailable",
                    "message": message,
                    "details": "private-secret",
                }
            },
        )

    m = native.Measurements("test-model", phase(), lambda _: None)
    with httpx.Client(transport=native.MeasuredTransport(m, httpx.MockTransport(provider), set())) as client:
        with pytest.raises(RuntimeError, match="Inference request rejected"):
            client.post(native.ROUTER + "/chat/completions", json={"model": "test-model"})
    assert m.failure == (
        "Inference request rejected (HTTP 503); reported code=token_accounting_unavailable"
        f"; reported accounting stage={stage}"
    )
    assert "private-secret" not in m.failure
    assert m.usage is None
    assert len(requests) == m.rounds == 1


def test_calendar_stage_is_not_trusted_under_unrelated_error_code():
    def provider(request):
        return httpx.Response(
            503,
            json={
                "error": {
                    "code": "unrelated",
                    "message": next(iter(native.CALENDAR_FAILURE_STAGES)),
                }
            },
        )

    m = native.Measurements("test-model", phase(), lambda _: None)
    with httpx.Client(transport=native.MeasuredTransport(m, httpx.MockTransport(provider), set())) as client:
        with pytest.raises(RuntimeError, match="reported code=unclassified"):
            client.post(native.ROUTER + "/chat/completions", json={"model": "test-model"})
    assert "accounting stage" not in m.failure
    assert m.usage is None and m.rounds == 1


@pytest.mark.parametrize(
    "body,stage,category",
    [
        (b"private-secret", "response JSON", "invalid value"),
        (b"\xff", "response JSON", "text encoding"),
        (b"null", "usage measurement", "invalid shape"),
        (b'{"choices":[]}', "completion shape", "invalid value"),
        (b'{"choices":null}', "completion shape", "invalid type"),
        (b'{"choices":[{"message":null}]}', "completion shape", "invalid shape"),
        (b'{"choices":[{"message":{"tool_calls":[{"function":null}]}}]}', "tool validation", "invalid shape"),
    ],
)
def test_response_failure_reports_only_fixed_stage_and_category(body, stage, category):
    m = native.Measurements("test-model", phase(), lambda _: None)
    transport = httpx.MockTransport(lambda _: httpx.Response(200, content=body))
    boundary = native.MeasuredTransport(m, transport, set())
    with httpx.Client(transport=boundary) as client:
        with pytest.raises((ValueError, UnicodeError, TypeError, AttributeError)):
            client.post(native.ROUTER + "/chat/completions", json={"model": "test-model"})
    assert m.failure == f"Inference response could not be measured; stage={stage}; category={category}"
    assert "private-secret" not in m.failure
    assert m.usage is None and m.rounds == 1


@pytest.mark.parametrize(
    "error,category",
    [
        (httpx.ReadTimeout("private-secret"), "timeout"),
        (httpx.DecodingError("private-secret"), "decoding"),
        (httpx.RemoteProtocolError("private-secret"), "HTTP transport"),
        (RuntimeError("private-secret"), "unclassified"),
    ],
)
def test_transport_failure_does_not_publish_exception_message(error, category):
    def provider(_):
        raise error

    m = native.Measurements("test-model", phase(), lambda _: None)
    with httpx.Client(transport=native.MeasuredTransport(m, httpx.MockTransport(provider), set())) as client:
        with pytest.raises(type(error)) as caught:
            client.post(native.ROUTER + "/chat/completions", json={"model": "test-model"})
    assert caught.value is error
    assert m.failure == f"Inference response could not be measured; stage=transport; category={category}"
    assert m.usage is None and m.rounds == 1


def test_progress_failure_is_not_misreported_as_provider_usage_failure():
    events = []

    def progress(event):
        events.append(event)
        if len(events) == 1:
            raise RuntimeError("private-secret")

    m = native.Measurements("test-model", phase(), progress)
    transport = httpx.MockTransport(
        lambda _: httpx.Response(
            200,
            json={
                "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
                "choices": [{"message": {"content": "private-secret"}, "finish_reason": "stop"}],
            },
        )
    )
    with httpx.Client(transport=native.MeasuredTransport(m, transport, set())) as client:
        with pytest.raises(RuntimeError):
            client.post(native.ROUTER + "/chat/completions", json={"model": "test-model"})
    assert m.failure == "Inference response could not be measured; stage=progress reporting; category=unclassified"
    assert len(events) == 2 and events[-1]["evidence"]["usage"] is None
    assert m.usage is None and m.rounds == 1
