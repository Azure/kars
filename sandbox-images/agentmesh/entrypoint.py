# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Launch an unchanged upstream AGT server directly, without a runtime shell."""

import os
import sys


def main():
    component = os.environ.get("AGENTMESH_COMPONENT")
    if component not in ("registry", "relay"):
        raise SystemExit("AGENTMESH_COMPONENT must be registry or relay")
    os.execv(sys.executable, [sys.executable, "-m", f"agentmesh.{component}", *sys.argv[1:]])


if __name__ == "__main__":
    main()
