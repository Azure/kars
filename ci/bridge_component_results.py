# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Require every declared Bridge component dependency to have succeeded."""

import json
import os


def require_success(results):
    if not isinstance(results, dict) or not results:
        raise ValueError("Bridge component results are missing or malformed")
    if any(not isinstance(value, dict) or value.get("result") != "success"
           for value in results.values()):
        raise ValueError("Every Bridge component, audit and add-on job must succeed")


if __name__ == "__main__":
    require_success(json.loads(os.environ["COMPONENT_RESULTS"]))
