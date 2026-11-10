# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Unit coverage for the image's Foundry reasoning projection."""

from __future__ import annotations

from copy import deepcopy
from pathlib import Path
import runpy
from types import ModuleType
import unittest
from unittest.mock import Mock, patch


SOURCE = Path(__file__).resolve().parents[3] / "sandbox-images/hermes/azure-foundry-provider.py"


class FoundryProviderTests(unittest.TestCase):
    def setUp(self):
        class ProviderProfile:
            def __init__(self, **kwargs):
                self.__dict__.update(kwargs)

            def build_api_kwargs_extras(self, **kwargs):
                return {}, {}

        providers = ModuleType("providers")
        providers.register_provider = Mock()
        base = ModuleType("providers.base")
        base.ProviderProfile = ProviderProfile
        reasoning = ModuleType("agent.reasoning_effort")
        reasoning.OPENAI_COMPAT_WIRE_EFFORTS = ("none", "low", "medium", "high", "max")
        reasoning.clamp_effort = Mock(side_effect=lambda effort, allowed: effort if effort in allowed else "max")
        with patch.dict("sys.modules", {
            "providers": providers, "providers.base": base, "agent.reasoning_effort": reasoning,
        }):
            self.module = runpy.run_path(str(SOURCE))
        self.profile = self.module["azure_foundry"]
        self.base = ProviderProfile
        self.register = providers.register_provider
        self.reasoning = reasoning

    def test_preserves_the_existing_provider_route_and_aliases(self):
        self.register.assert_called_once_with(self.profile)
        self.assertEqual(self.profile.name, "azure-foundry")
        self.assertEqual(self.profile.aliases, ("azure", "azure-ai-foundry", "azure-ai"))
        self.assertEqual(self.profile.env_vars, ("AZURE_FOUNDRY_API_KEY", "AZURE_FOUNDRY_BASE_URL"))
        self.assertEqual(self.profile.base_url, "")
        self.assertEqual(self.profile.auth_type, "api_key")

    def test_override_suppresses_the_native_generic_nested_fallback(self):
        self.assertIsNot(type(self.profile).build_api_kwargs_extras, self.base.build_api_kwargs_extras)

    def test_title_generation_disables_reasoning_on_the_standard_wire(self):
        self.assertEqual(
            self.profile.build_api_kwargs_extras(reasoning_config={"enabled": False}),
            ({}, {"reasoning_effort": "none"}),
        )
        self.reasoning.clamp_effort.assert_not_called()

    def test_disabled_reasoning_wins_over_an_effort(self):
        self.assertEqual(
            self.profile.build_api_kwargs_extras(reasoning_config={"enabled": False, "effort": "high"}),
            ({}, {"reasoning_effort": "none"}),
        )

    def test_explicit_none_disables_reasoning(self):
        self.assertEqual(
            self.profile.build_api_kwargs_extras(reasoning_config={"effort": " NONE "}),
            ({}, {"reasoning_effort": "none"}),
        )

    def test_unspecified_effort_preserves_the_provider_default(self):
        for config in (None, {}, {"enabled": True}, {"effort": ""}):
            with self.subTest(config=config):
                self.assertEqual(self.profile.build_api_kwargs_extras(reasoning_config=config), ({}, {}))
        self.reasoning.clamp_effort.assert_not_called()

    def test_enabled_effort_uses_the_native_wire_clamp_without_mutating_input(self):
        config = {"enabled": True, "effort": " ULTRA "}
        original = deepcopy(config)
        self.assertEqual(
            self.profile.build_api_kwargs_extras(reasoning_config=config),
            ({}, {"reasoning_effort": "max"}),
        )
        self.reasoning.clamp_effort.assert_called_once_with("ultra", self.reasoning.OPENAI_COMPAT_WIRE_EFFORTS)
        self.assertEqual(config, original)

    def test_model_and_transport_hints_do_not_add_vendor_extensions(self):
        self.assertEqual(
            self.profile.build_api_kwargs_extras(
                reasoning_config={"enabled": False}, model="claude-opus-5.5",
                base_url="http://127.0.0.1:8443/v1", ollama_num_ctx=1000,
            ),
            ({}, {"reasoning_effort": "none"}),
        )


if __name__ == "__main__":
    unittest.main()
