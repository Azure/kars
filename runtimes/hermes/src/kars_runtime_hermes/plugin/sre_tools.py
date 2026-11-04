# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Register SRE argument schemas in Hermes' native function envelope."""

from collections.abc import Callable
from typing import Any


def register_sre_tool(
    ctx: Any,
    *,
    name: str,
    toolset: str,
    description: str,
    schema: dict[str, Any],
    handler: Callable[..., str],
) -> None:
    ctx.register_tool(
        name=name,
        toolset=toolset,
        description=description,
        schema={"name": name, "description": description, "parameters": schema},
        handler=handler,
    )
