# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import pytest

from kars_runtime_hermes.plugin import mission_protocol as wire


@pytest.fixture
def model_metadata(monkeypatch):
    import httpx
    from kars_runtime_hermes.plugin import mission_receiver

    state = {"status": 200, "body": {"data": [{"id": "mission-model", "context_window": 1_050_000,
                                               "max_output_tokens": 128_000}]}, "requests": []}
    factory = httpx.AsyncClient

    def handler(request):
        assert request.method == "GET" and str(request.url) == "http://127.0.0.1:8443/v1/models"
        state["requests"].append(request)
        return httpx.Response(state["status"], json=state["body"])

    def client(**kwargs):
        kwargs.setdefault("transport", httpx.MockTransport(handler))
        return factory(**kwargs)

    monkeypatch.setattr(mission_receiver.httpx, "AsyncClient", client)
    return state


@pytest.fixture
def input_artifact():
    def create(name, content="Complete immutable handoff"):
        return {"name": name, "content": content, "sha256": wire.text_hash(content), "source": {
            "namespace": "kars-system", "taskName": "writer", "taskUid": "upstream-task",
            "sandboxUid": "upstream-sandbox", "podUid": "upstream-pod", "runNonce": "upstream-run",
            "assignmentId": "upstream-assignment", "agentDid": "did:mesh:" + "a" * 32,
            "artifactName": name}}
    return create
