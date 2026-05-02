# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import os

from azureclaw_runtime_maf_python import runtime


def test_bootstrap_sets_initialized_env(monkeypatch):
    monkeypatch.delenv(runtime.ENV_INITIALIZED, raising=False)
    runtime.bootstrap()
    assert os.environ.get(runtime.ENV_INITIALIZED) == "1"


def test_bootstrap_sets_openai_base_url(monkeypatch):
    monkeypatch.delenv("OPENAI_BASE_URL", raising=False)
    monkeypatch.delenv(runtime.ENV_INITIALIZED, raising=False)
    runtime.bootstrap()
    assert os.environ["OPENAI_BASE_URL"] == runtime.DEFAULT_OPENAI_BASE_URL


def test_bootstrap_sets_azure_openai_endpoint(monkeypatch):
    monkeypatch.delenv("AZURE_OPENAI_ENDPOINT", raising=False)
    monkeypatch.delenv(runtime.ENV_INITIALIZED, raising=False)
    runtime.bootstrap()
    assert (
        os.environ["AZURE_OPENAI_ENDPOINT"]
        == runtime.DEFAULT_AZURE_OPENAI_ENDPOINT
    )


def test_bootstrap_does_not_clobber_existing_endpoints(monkeypatch):
    monkeypatch.setenv("OPENAI_BASE_URL", "http://router/openai/v1")
    monkeypatch.setenv("AZURE_OPENAI_ENDPOINT", "http://router/azure")
    monkeypatch.delenv(runtime.ENV_INITIALIZED, raising=False)
    runtime.bootstrap()
    assert os.environ["OPENAI_BASE_URL"] == "http://router/openai/v1"
    assert os.environ["AZURE_OPENAI_ENDPOINT"] == "http://router/azure"


def test_bootstrap_idempotent(monkeypatch, mocker):
    monkeypatch.delenv(runtime.ENV_INITIALIZED, raising=False)
    spy = mocker.spy(runtime, "init_telemetry")
    runtime.bootstrap()
    runtime.bootstrap()
    assert spy.call_count == 1


def test_bootstrap_swallows_init_telemetry_errors(monkeypatch, mocker):
    monkeypatch.delenv(runtime.ENV_INITIALIZED, raising=False)
    mocker.patch.object(runtime, "init_telemetry", side_effect=RuntimeError("boom"))
    runtime.bootstrap()
    assert os.environ[runtime.ENV_INITIALIZED] == "1"
