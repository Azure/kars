# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pinned Hermes oneshot/middleware/SDK path; only the HTTP boundary is simulated."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

import httpx
import pytest

pytestmark = pytest.mark.skipif(
    importlib.util.find_spec("run_agent") is None, reason="Requires native pinned Hermes",
)


@pytest.mark.parametrize("enabled,toolset", [(True, "sre"), (False, "sre"), (True, "unknown-sre")])
def test_cold_cli_discovers_staged_sre_plugin_without_broadening_tools(tmp_path, enabled, toolset):
    from kars_runtime_hermes import plugin

    source = Path("/opt/kars-hermes-stage/plugins/kars")
    if not source.is_dir():
        source = Path(plugin.__file__).parent
    assert (source / "plugin.yaml").is_file(), "Staged Kars plugin discovery manifest is missing"
    home = tmp_path / "hermes"
    shutil.copytree(source, home / "plugins" / "kars")
    (home / "config.yaml").write_text(
        f"plugins:\n  enabled: {['kars'] if enabled else []}\n"
        "model:\n  default: gpt-5.4-mini\n  provider: azure-foundry\n"
        "  base_url: http://127.0.0.1:8443/v1\n"
    )
    env = {
        **os.environ, "HOME": str(tmp_path), "HERMES_HOME": str(home), "TMPDIR": str(tmp_path),
        "SRE_ENABLED": "true", "KARS_SRE_ENABLED": "true", "HERMES_TUI": "0",
        "HERMES_TELEMETRY_ENABLED": "false", "HERMES_DISABLE_LAZY_INSTALLS": "1",
        "HERMES_SKIP_NODE_BOOTSTRAP": "1", "PYTHONDONTWRITEBYTECODE": "1",
        "AZURE_FOUNDRY_BASE_URL": "http://127.0.0.1:8443/v1",
        "AZURE_FOUNDRY_API_KEY": "router-managed",
    }
    script = '''import sys
from hermes_cli import oneshot

def admitted(prompt, **kwargs):
    assert kwargs["toolsets"] == ["sre"], kwargs
    assert kwargs["provider"] == "azure-foundry", kwargs
    assert kwargs["model"] == "gpt-5.4-mini", kwargs
    from toolsets import resolve_toolset
    assert set(resolve_toolset("sre")) == {
        "sre_describe_state", "sre_logs", "sre_diagnose", "sre_explain_error", "sre_propose_fix",
        "sre_describe_resource", "sre_what_changed", "sre_endpoints_inspect", "sre_image_probe", "sre_top",
    }
    return "COLD_SRE_SCOPE_ADMITTED", {"completed": True}

# Only agent execution is replaced; CLI parsing, discovery and validation remain native.
oneshot._run_agent = admitted
sys.argv = ["hermes", "--accept-hooks", "--cli", "--provider", "azure-foundry",
            "--model", "gpt-5.4-mini", "--toolsets", sys.argv[1], "-z", "Check restricted SRE scope."]
from hermes_cli.main import main
main()
'''
    result = subprocess.run(
        [sys.executable, "-B", "-c", script, toolset], cwd=tmp_path, env=env,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=90,
    )
    if enabled and toolset == "sre":
        assert result.returncode == 0, result.stdout
        assert "COLD_SRE_SCOPE_ADMITTED" in result.stdout
    else:
        assert result.returncode != 0, result.stdout
        assert "--toolsets did not contain any valid toolsets" in result.stdout
        assert "COLD_SRE_SCOPE_ADMITTED" not in result.stdout


@pytest.mark.parametrize("bounded", [False, True])
@pytest.mark.parametrize("model", ["gpt-5.4-mini", "gpt-6.1-sol"])
def test_native_sre_oneshot_serializes_default_output_bound(tmp_path, monkeypatch, bounded, model):
    from hermes_cli import plugins
    from hermes_cli.oneshot import _run_agent
    from kars_runtime_hermes.plugin import sre

    home = tmp_path / "home"
    home.mkdir()
    (home / "config.yaml").write_text(
        f"model:\n  default: {model}\n  provider: azure-foundry\n"
        "  base_url: http://127.0.0.1:8443/v1\n  context_length: 200000\n"
        "agent:\n  reasoning_effort: high\n"
        "compression:\n  enabled: false\nmemory:\n  memory_enabled: false\n"
    )
    for key in ("HOME", "HERMES_HOME", "TMPDIR"):
        monkeypatch.setenv(key, str(home))
    for key, value in {
        "AZURE_FOUNDRY_BASE_URL": "http://127.0.0.1:8443/v1",
        "AZURE_FOUNDRY_API_KEY": "router-managed",
        "HERMES_TELEMETRY_ENABLED": "false",
        "HERMES_DISABLE_LAZY_INSTALLS": "1",
        "HERMES_SKIP_NODE_BOOTSTRAP": "1",
        "HERMES_ACCEPT_HOOKS": "1",
        "TIRITH_ENABLED": "false",
    }.items():
        monkeypatch.setenv(key, value)
    monkeypatch.chdir(home)
    manager = plugins.PluginManager()
    monkeypatch.setattr(plugins, "get_plugin_manager", lambda: manager)
    ctx = plugins.PluginContext(plugins.PluginManifest(name="kars-sre-test"), manager)
    sre.register(ctx)
    if not bounded:
        manager._middleware.clear()
    seen = []

    def send(_client, request, **_kwargs):
        assert request.url.host == "127.0.0.1" and request.url.port == 8443
        body = json.loads(request.content)
        if request.url.path == "/v1/chat/completions":
            assert not body.get("tools"), "The main tool turn must use Responses, not Chat Completions"
            return httpx.Response(200, request=request, json={
                "id": "title-sre", "object": "chat.completion", "created": 1, "model": model,
                "choices": [{"index": 0, "finish_reason": "stop", "message": {
                    "role": "assistant", "content": "SRE request-bound test"}}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
            })
        assert request.url.path == "/v1/responses"
        seen.append(body)
        item = {"id": "msg-sre", "type": "message", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": "No cluster checks requested.",
                             "annotations": []}]}
        response = {"id": "resp-sre", "object": "response", "created_at": 1,
                    "status": "completed", "model": model, "output": [item],
                    "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}}
        if not body.get("stream"):
            return httpx.Response(200, request=request, json=response)
        events = [
            {"type": "response.created", "response": {**response, "output": [], "status": "in_progress"}},
            {"type": "response.output_item.added", "output_index": 0,
             "item": {**item, "status": "in_progress", "content": []}},
            {"type": "response.content_part.added", "item_id": item["id"], "output_index": 0,
             "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}},
            {"type": "response.output_text.delta", "item_id": item["id"], "output_index": 0,
             "content_index": 0, "delta": item["content"][0]["text"]},
            {"type": "response.output_text.done", "item_id": item["id"], "output_index": 0,
             "content_index": 0, "text": item["content"][0]["text"]},
            {"type": "response.content_part.done", "item_id": item["id"], "output_index": 0,
             "content_index": 0, "part": item["content"][0]},
            {"type": "response.output_item.done", "output_index": 0, "item": item},
            {"type": "response.completed", "response": response},
        ]
        return httpx.Response(200, request=request, headers={"content-type": "text/event-stream"},
                              text="".join("data: " + json.dumps({**event, "sequence_number": index})
                                           + "\n\n" for index, event in enumerate(events)))

    monkeypatch.setattr(httpx.Client, "send", send)
    output, result = _run_agent(
        "Reply 'No cluster checks requested.' without calling tools.",
        model=model, provider="azure-foundry", toolsets=["sre"], use_config_toolsets=False,
    )
    assert result.get("completed") is True, result
    assert output == "No cluster checks requested."
    main_requests = [request for request in seen if "tools" in request]
    assert len(main_requests) == 1
    for request in main_requests:
        assert request["model"] == model
        assert request["reasoning"]["effort"] == "high"
        if bounded:
            assert request["max_output_tokens"] == 8192
        else:
            assert "max_output_tokens" not in request
    assert 400000 + (8192 if bounded else 128000) == (408192 if bounded else 528000)
