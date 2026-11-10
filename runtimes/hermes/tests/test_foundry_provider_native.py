# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pinned Hermes auxiliary builder and real OpenAI serialization; HTTP is simulated."""

import importlib.util
import json

import httpx
import pytest

pytestmark = pytest.mark.skipif(
    importlib.util.find_spec("run_agent") is None, reason="Requires native pinned Hermes",
)


@pytest.fixture
def native_home(tmp_path, monkeypatch):
    monkeypatch.setenv("HERMES_HOME", str(tmp_path))
    monkeypatch.setenv("HERMES_TELEMETRY_ENABLED", "false")
    monkeypatch.setenv("HERMES_DISABLE_LAZY_INSTALLS", "1")
    monkeypatch.setenv("HERMES_SKIP_NODE_BOOTSTRAP", "1")


@pytest.mark.parametrize("model", ["claude-opus-5.5", "gpt-5.4-mini", "gpt-6.1-sol"])
def test_native_title_request_is_bounded_and_openai_compatible(native_home, model):
    from agent.auxiliary_client import _build_call_kwargs
    from agent.title_generator import _TITLE_RESPONSE_FORMAT
    from openai import OpenAI
    from providers import get_provider_profile
    from providers.base import ProviderProfile

    profile = get_provider_profile("azure-foundry")
    assert type(profile).build_api_kwargs_extras is not ProviderProfile.build_api_kwargs_extras
    kwargs = _build_call_kwargs(
        provider="azure-foundry", model=model,
        messages=[{"role": "user", "content": "Name this diagnostic session."}],
        max_tokens=512, temperature=None, task="title_generation",
        base_url="http://127.0.0.1:8443/v1", reasoning_config={"enabled": False},
        extra_body={"response_format": _TITLE_RESPONSE_FORMAT},
    )
    requests = []

    def send(request):
        assert str(request.url) == "http://127.0.0.1:8443/v1/chat/completions"
        body = json.loads(request.content)
        requests.append(body)
        assert body.get("max_tokens", body.get("max_completion_tokens")) == 512
        assert not ("max_tokens" in body and "max_completion_tokens" in body)
        assert body["reasoning_effort"] == "none"
        assert "reasoning" not in body
        assert "temperature" not in body
        assert body["response_format"] == _TITLE_RESPONSE_FORMAT
        return httpx.Response(200, request=request, json={
            "id": "title-wire", "object": "chat.completion", "created": 1, "model": model,
            "choices": [{"index": 0, "finish_reason": "stop", "message": {
                "role": "assistant", "content": '{"title":"Diagnostic session"}'}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 6, "total_tokens": 18},
        })

    with httpx.Client(transport=httpx.MockTransport(send)) as transport:
        with OpenAI(api_key="router-managed", base_url="http://127.0.0.1:8443/v1",
                    http_client=transport, max_retries=0) as client:
            result = client.chat.completions.create(**kwargs)
    assert result.usage.total_tokens == 18
    assert len(requests) == 1


@pytest.mark.parametrize("config,expected", [
    (None, None), ({}, None), ({"enabled": False}, "none"),
    ({"effort": "high"}, "high"), ({"effort": "ultra"}, "max"),
])
def test_native_profile_uses_the_real_effort_clamp(native_home, config, expected):
    from providers import get_provider_profile

    extra, top = get_provider_profile("azure-foundry").build_api_kwargs_extras(reasoning_config=config)
    assert extra == {}
    assert top == ({} if expected is None else {"reasoning_effort": expected})


@pytest.mark.parametrize("provider", ["azure-foundry", "azure", "azure-ai-foundry", "azure-ai"])
def test_native_foundry_aliases_preserve_only_explicit_caps(native_home, provider):
    from agent.auxiliary_client import _build_call_kwargs

    for cap in (None, 512):
        kwargs = _build_call_kwargs(
            provider=provider, model="claude-opus-5.5", messages=[], max_tokens=cap,
            base_url="http://127.0.0.1:8443/v1", task="title_generation",
        )
        assert kwargs.get("max_tokens", kwargs.get("max_completion_tokens")) == cap
        assert "reasoning" not in kwargs.get("extra_body", {})


@pytest.mark.parametrize("model,expected", [
    ("gpt-6.1-sol", "codex_responses"),
    ("azure/gpt-6.1-sol", "codex_responses"),
    ("gpt-5.4-mini", "codex_responses"),
    ("o3", "codex_responses"),
    ("gpt-4o", "chat_completions"),
    ("claude-opus-5.5", "chat_completions"),
    ("gpt-6-unknown", "chat_completions"),
])
def test_native_foundry_api_selection_preserves_other_routes(native_home, model, expected):
    from hermes_cli.runtime_provider import _azure_inferred_api_mode

    assert _azure_inferred_api_mode(model, "chat_completions") == expected
    assert _azure_inferred_api_mode(model, "anthropic_messages") == "anthropic_messages"


def test_unrelated_generic_provider_cap_behavior_is_unchanged(native_home):
    from agent.auxiliary_client import _forwards_max_tokens

    assert not _forwards_max_tokens("fireworks", "fireworks", "example", "", "title_generation")
