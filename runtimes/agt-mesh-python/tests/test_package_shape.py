# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Tests for the public package shape."""

from __future__ import annotations

import pytest

import kars_agt_mesh


def test_public_exports_present() -> None:
    """Smoke test: the documented public API names import without error."""
    for name in (
        "MeshClient",
        "MeshConfig",
        "InboundMessage",
        "KarsAgtMeshError",
        "MeshAuthError",
        "MeshPeerNotFoundError",
        "MeshRegistryError",
        "MeshTransportError",
        "MeshTrustGateError",
    ):
        assert hasattr(kars_agt_mesh, name), name


def test_version() -> None:
    assert kars_agt_mesh.__version__ == "0.1.0"


def test_config_validates_name() -> None:
    from kars_agt_mesh import MeshConfig

    with pytest.raises(ValueError, match="DNS-label"):
        MeshConfig(
            name="UPPERCASE",
            relay_url="ws://r",
            registry_url="http://r",
            identity_path="/tmp/i.json",
        )
    with pytest.raises(ValueError, match="1\\.\\.63 chars"):
        MeshConfig(
            name="",
            relay_url="ws://r",
            registry_url="http://r",
            identity_path="/tmp/i.json",
        )
    with pytest.raises(ValueError, match="1\\.\\.63 chars"):
        MeshConfig(
            name="x" * 64,
            relay_url="ws://r",
            registry_url="http://r",
            identity_path="/tmp/i.json",
        )


def test_config_validates_urls() -> None:
    from kars_agt_mesh import MeshConfig

    with pytest.raises(ValueError, match="ws://"):
        MeshConfig(
            name="a",
            relay_url="http://wrong",
            registry_url="http://r",
            identity_path="/tmp/i.json",
        )
    with pytest.raises(ValueError, match="http://"):
        MeshConfig(
            name="a",
            relay_url="ws://r",
            registry_url="ws://wrong",
            identity_path="/tmp/i.json",
        )
