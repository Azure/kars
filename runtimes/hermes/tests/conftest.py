# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import pytest

from kars_runtime_hermes.plugin import mission_protocol as wire


@pytest.fixture
def input_artifact():
    def create(name, content="Complete immutable handoff"):
        return {"name": name, "content": content, "sha256": wire.text_hash(content), "source": {
            "namespace": "kars-system", "taskName": "writer", "taskUid": "upstream-task",
            "sandboxUid": "upstream-sandbox", "podUid": "upstream-pod", "runNonce": "upstream-run",
            "assignmentId": "upstream-assignment", "agentDid": "did:mesh:" + "a" * 32,
            "artifactName": name}}
    return create
