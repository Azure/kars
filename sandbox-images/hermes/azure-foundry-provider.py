# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Hermes Foundry profile with OpenAI-compatible reasoning controls."""

from typing import Any

from agent.reasoning_effort import OPENAI_COMPAT_WIRE_EFFORTS, clamp_effort
from providers import register_provider
from providers.base import ProviderProfile


class AzureFoundryProfile(ProviderProfile):
    def build_api_kwargs_extras(
        self, *, reasoning_config: dict | None = None, **ctx: Any
    ) -> tuple[dict[str, Any], dict[str, Any]]:
        # Declaring this hook also suppresses Hermes' generic nested reasoning fallback.
        top_level: dict[str, Any] = {}
        if isinstance(reasoning_config, dict) and reasoning_config:
            effort = (reasoning_config.get("effort") or "").strip().lower()
            if reasoning_config.get("enabled", True) is False or effort == "none":
                top_level["reasoning_effort"] = "none"
            elif effort:
                top_level["reasoning_effort"] = clamp_effort(effort, OPENAI_COMPAT_WIRE_EFFORTS)
        return {}, top_level


azure_foundry = AzureFoundryProfile(
    name="azure-foundry", aliases=("azure", "azure-ai-foundry", "azure-ai"), display_name="Azure Foundry",
    description="Microsoft Foundry - OpenAI-compatible endpoint (user-supplied base URL)",
    signup_url="https://ai.azure.com/", env_vars=("AZURE_FOUNDRY_API_KEY", "AZURE_FOUNDRY_BASE_URL"),
    base_url="",
    auth_type="api_key",
)

register_provider(azure_foundry)
