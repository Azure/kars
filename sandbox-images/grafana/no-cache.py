# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Use Go's supported no-get/no-put cache mode for a single bounded build."""
import json
import sys

print(json.dumps({"ID": 0, "KnownCommands": ["close"]}), flush=True)
for line in sys.stdin:
    if not line.strip():
        continue
    request = json.loads(line)
    if request.get("Command") != "close":
        raise RuntimeError("Unexpected command in no-storage build-cache mode")
    print(json.dumps({"ID": request["ID"]}), flush=True)
    break
else:
    raise RuntimeError("Build-cache client exited without closing")
