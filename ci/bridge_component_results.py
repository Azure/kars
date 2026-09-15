# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Require the complete Bridge component qualification graph to succeed."""

import json
import os


REQUIRED_JOBS = frozenset({
    "addon", "bff", "dependencies", "lockfiles",
    "rust-dependencies", "secrets", "security", "web",
})


def require_success(results):
    if not isinstance(results, dict) or results.keys() != REQUIRED_JOBS:
        raise ValueError("Bridge component results must include exactly the required jobs")
    if any(not isinstance(value, dict) or value.get("result") != "success"
           for value in results.values()):
        raise ValueError("Every Bridge component, audit and add-on job must succeed")


if __name__ == "__main__":
    require_success(json.loads(os.environ["COMPONENT_RESULTS"]))
