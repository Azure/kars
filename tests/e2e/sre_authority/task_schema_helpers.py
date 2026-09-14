# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json

from .common import require


def payload_helper(h, mode, value=None):
    args = ["node", str(h.root / "tests/e2e/sre_authority/task_schema_payload.mjs"), mode]
    return json.loads(h.run(args, **({"data": json.dumps(value)} if value is not None else {}), timeout=20))


def require_task_payload_helper(h):
    require((h.root / "cli/dist/lib/schema-write-request.js").is_file(),
            "Early Task SSA requires Node.js 22+ and a CLI build: run npm ci && npm run build in cli before the schema tests")
    require(payload_helper(h, "check") == {"ready": True}, "Compiled production schema helper is unavailable")
