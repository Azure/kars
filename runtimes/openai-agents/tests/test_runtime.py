# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import os

from azureclaw_runtime_openai_agents import runtime


def test_bootstrap_sets_initialized_env(monkeypatch):
    monkeypatch.delenv(runtime.ENV_INITIALIZED, raising=False)
    runtime.bootstrap()
    assert os.environ.get(runtime.ENV_INITIALIZED) == "1"


def test_bootstrap_sets_openai_base_url(monkeypatch):
    monkeypatch.delenv("OPENAI_BASE_URL", raising=False)
    monkeypatch.delenv(runtime.ENV_INITIALIZED, raising=False)
    runtime.bootstrap()
    assert os.environ["OPENAI_BASE_URL"] == runtime.DEFAULT_OPENAI_BASE_URL


def test_bootstrap_does_not_clobber_existing_base_url(monkeypatch):
    monkeypatch.setenv("OPENAI_BASE_URL", "http://router/openai/v1")
    monkeypatch.delenv(runtime.ENV_INITIALIZED, raising=False)
    runtime.bootstrap()
    assert os.environ["OPENAI_BASE_URL"] == "http://router/openai/v1"


def test_bootstrap_idempotent(monkeypatch, mocker):
    monkeypatch.delenv(runtime.ENV_INITIALIZED, raising=False)
    spy = mocker.spy(runtime, "init_telemetry")
    runtime.bootstrap()
    runtime.bootstrap()  # second call should short-circuit
    assert spy.call_count == 1


def test_bootstrap_swallows_init_telemetry_errors(monkeypatch, mocker):
    monkeypatch.delenv(runtime.ENV_INITIALIZED, raising=False)
    mocker.patch.object(runtime, "init_telemetry", side_effect=RuntimeError("boom"))
    # Must not propagate.
    runtime.bootstrap()
    assert os.environ[runtime.ENV_INITIALIZED] == "1"
