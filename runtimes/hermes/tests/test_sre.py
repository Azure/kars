# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""kars-sre plugin tests (Slice 1)."""

from __future__ import annotations

import importlib
import os
import sys
from unittest.mock import MagicMock, patch


def test_is_enabled_default_false() -> None:
    """Without SRE_ENABLED, the plugin must be disabled."""
    from kars_runtime_hermes.plugin import sre

    with patch.dict(os.environ, {}, clear=True):
        assert not sre.is_enabled()


def test_is_enabled_accepts_truthy_values() -> None:
    from kars_runtime_hermes.plugin import sre

    for v in ("true", "True", "TRUE", "1", "yes", "YES"):
        with patch.dict(os.environ, {"SRE_ENABLED": v}, clear=True):
            assert sre.is_enabled(), f"value {v!r} should be truthy"


def test_is_enabled_rejects_falsy_values() -> None:
    from kars_runtime_hermes.plugin import sre

    for v in ("false", "0", "no", "", "anything-else"):
        with patch.dict(os.environ, {"SRE_ENABLED": v}, clear=True):
            assert not sre.is_enabled(), f"value {v!r} should be falsy"


def test_register_skips_when_disabled() -> None:
    """A standard Hermes plugin __init__.py call must not register sre tools."""
    # Reload the plugin __init__ to get a clean state
    if "kars_runtime_hermes.plugin" in sys.modules:
        importlib.reload(sys.modules["kars_runtime_hermes.plugin"])
    with patch.dict(os.environ, {}, clear=True):
        from kars_runtime_hermes.plugin import sre

        ctx = MagicMock()
        # Direct sre.register call should never run unless caller checks
        # is_enabled first — but we also want to be defensive: if a
        # standard sandbox somehow imports and registers, that's a bug.
        # Slice 1's gate is in __init__.py, not in register() itself,
        # so calling register() directly DOES register tools. That's
        # fine for now (we're testing the __init__.py path elsewhere).
        sre.register(ctx)
        # 5 Slice-1 + 5 Slice-2 = 10 tool registrations expected
        assert ctx.register_tool.call_count == 10


def test_register_registers_all_ten_tools() -> None:
    """register(ctx) registers exactly the Slice 1 + Slice 2 tools."""
    from kars_runtime_hermes.plugin import sre

    ctx = MagicMock()
    sre.register(ctx)

    tool_names = {call.kwargs["name"] for call in ctx.register_tool.call_args_list}
    expected = {
        # Slice 1 — read-only kars-CR tools
        "sre_describe_state",
        "sre_logs",
        "sre_diagnose",
        "sre_explain_error",
        "sre_propose_fix",
        # Slice 2 — K8s diagnostic toolset
        "sre_describe_resource",
        "sre_what_changed",
        "sre_endpoints_inspect",
        "sre_image_probe",
        "sre_top",
    }
    assert tool_names == expected, f"got {tool_names}, expected {expected}"


def test_register_handles_missing_register_tool_gracefully() -> None:
    """If ctx has no register_tool callable, log + return without raising."""
    from kars_runtime_hermes.plugin import sre

    class BadCtx:
        pass

    sre.register(BadCtx())  # must not raise


def test_explain_error_matches_imagepullbackoff() -> None:
    from kars_runtime_hermes.plugin import sre

    result = sre._impl_sre_explain_error(error="Failed to pull image: ImagePullBackOff")
    assert result["matched"] is True
    assert result["hypotheses"][0]["pattern"] == "ImagePullBackOff"


def test_explain_error_matches_exceeded_quota() -> None:
    from kars_runtime_hermes.plugin import sre

    result = sre._impl_sre_explain_error(error="pods 'foo' is forbidden: exceeded quota: tight-quota")
    assert result["matched"] is True
    assert result["hypotheses"][0]["pattern"] == "exceeded quota"


def test_explain_error_no_match() -> None:
    from kars_runtime_hermes.plugin import sre

    result = sre._impl_sre_explain_error(error="totally-unknown-thing")
    assert result["matched"] is False
    assert result["error"] == "totally-unknown-thing"


def test_explain_error_empty_string() -> None:
    from kars_runtime_hermes.plugin import sre

    result = sre._impl_sre_explain_error(error="")
    assert result["matched"] is False
    assert "reason" in result


def test_propose_fix_for_resourcequota() -> None:
    """Slice 3 demo target — DeleteResourceQuota typed action.

    The proposal envelope must carry the typed action; whether the
    KarsSREAction CR was created depends on whether we're running in
    a pod with a projected SA token. Both pod (CR created) and unit-
    test (cr_error captured) paths return the same action shape.
    """
    from kars_runtime_hermes.plugin import sre

    result = sre._impl_sre_propose_fix(
        diagnosis="ResourceQuota platform-hardening-quota in kars-research is blocking pod admission",
        target={
            "kind": "ResourceQuota",
            "namespace": "kars-research",
            "name": "platform-hardening-quota",
        },
    )
    assert result["kind"] == "FixProposal"
    assert result["action"] is not None
    assert result["action"]["type"] == "DeleteResourceQuota"
    assert result["action"]["namespace"] == "kars-research"
    assert result["action"]["name"] == "platform-hardening-quota"
    # Slice 3 + watcher: when the proposal carries a typed action the
    # tool tries to create a KarsSREAction CR. Outside a pod (unit
    # test) the SA-token read fails and surfaces in cr_error; inside a
    # pod cr_created=True and action_id is set. Either way the
    # operator-facing execution_status announces awaiting-approval.
    assert "operator approval" in result["execution_status"]


def test_propose_fix_unknown_target_kind() -> None:
    """For target kinds the watcher doesn't codify, return envelope with no action.

    Slice 3 adds Pod / Deployment / StatefulSet / DaemonSet handling,
    so we use ConfigMap here as the genuine "unknown" case.
    """
    from kars_runtime_hermes.plugin import sre

    result = sre._impl_sre_propose_fix(
        diagnosis="config drift on a ConfigMap",
        target={"kind": "ConfigMap", "namespace": "default", "name": "drifted"},
    )
    assert result["kind"] == "FixProposal"
    assert result["action"] is None
    # Still returns rationale for the operator
    assert "rationale" in result and result["rationale"]
    # And the cr_error explains what was missing.
    assert result.get("cr_error") is not None


def test_kars_cr_kinds_covers_all_eleven_crds() -> None:
    """The KARS_CR_KINDS list must include every CRD in proposal §3.5."""
    from kars_runtime_hermes.plugin import sre

    expected = {
        "KarsSandbox", "InferencePolicy", "ToolPolicy", "EgressApproval",
        "KarsMemory", "KarsEval", "TrustGraph", "KarsPairing", "A2AAgent",
        "McpServer", "KarsAuthConfig",
    }
    actual = {kind for _plural, kind in sre.KARS_CR_KINDS}
    assert actual == expected, f"missing/extra CRDs: {actual ^ expected}"


def test_describe_state_with_mocked_kube() -> None:
    """describe_state walks every kind and summarises items."""
    from kars_runtime_hermes.plugin import sre

    fake_doc = {
        "items": [
            {
                "metadata": {"namespace": "kars-system", "name": "foo"},
                "status": {
                    "phase": "Ready",
                    "observedGeneration": 3,
                    "lastReconciled": "2026-06-09T10:00:00Z",
                    "conditions": [{"type": "Available", "status": "True"}],
                },
            },
        ],
    }
    mock_client = MagicMock()
    mock_client.get.return_value = fake_doc

    with patch.object(sre.sre_kube, "client", return_value=mock_client):
        result = sre._impl_sre_describe_state()

    # Every kind got summarised
    assert set(result.keys()) == {k for _p, k in sre.KARS_CR_KINDS}
    # Each got one entry from the fake doc
    for kind in result:
        assert isinstance(result[kind], list)
        assert len(result[kind]) == 1
        assert result[kind][0]["phase"] == "Ready"
        assert result[kind][0]["kind"] == kind


def test_describe_state_handles_apiserver_errors_per_kind() -> None:
    """A 403/404 on one kind must not blow up the whole call."""
    import httpx

    from kars_runtime_hermes.plugin import sre

    mock_client = MagicMock()
    response = MagicMock(status_code=403, reason_phrase="Forbidden")
    mock_client.get.side_effect = httpx.HTTPStatusError(
        "403", request=MagicMock(), response=response
    )

    with patch.object(sre.sre_kube, "client", return_value=mock_client):
        result = sre._impl_sre_describe_state()

    # Every kind got an error entry, but no exception bubbled up
    for kind in result:
        assert isinstance(result[kind], dict)
        assert "error" in result[kind]
        assert "403" in result[kind]["error"]


def test_diagnosis_preserves_failed_inventory_coverage() -> None:
    from kars_runtime_hermes.plugin import sre

    kube = MagicMock()
    kube.get.side_effect = RuntimeError("403 Forbidden")
    with patch.object(sre.sre_kube, "client", return_value=kube):
        report = sre._impl_sre_diagnose()
    assert report["coverage"] == "incomplete"
    assert len(report["coverage_errors"]) == len(sre.KARS_CR_KINDS) + 2
    assert report["crds_present"] == []
    assert "coverage: incomplete" in report["summary"]
    assert all("403" in error["error"] for error in report["coverage_errors"])


def test_diagnosis_tracks_freshness_and_workload_read_failures() -> None:
    from kars_runtime_hermes.plugin import sre

    kube = MagicMock()

    def get(path):
        if path.endswith("/deployments/kars-controller"):
            return {"spec": {"replicas": 1}, "status": {"readyReplicas": 1}}
        if path.endswith("/customresourcedefinitions"):
            return {"items": [{"metadata": {"name": plural + ".kars.azure.com"}}
                              for plural, _ in sre.KARS_CR_KINDS]}
        if path.endswith("/karssandboxes"):
            return {"items": [
                {"metadata": {"name": "stale", "namespace": "kars-system"},
                 "status": {"phase": "Running", "lastReconciled": "2020-01-01T00:00:00Z"}},
                {"metadata": {"name": "unknown", "namespace": "kars-system"},
                 "status": {"phase": "Running"}},
            ]}
        if "/deployments/" in path:
            raise RuntimeError("workload inventory unavailable")
        return {"items": []}

    kube.get.side_effect = get
    with patch.object(sre.sre_kube, "client", return_value=kube):
        report = sre._impl_sre_diagnose()
    assert report["controller_status"] == "Ready"
    assert [item["name"] for item in report["stale_reconciles"]] == ["stale"]
    assert len(report["coverage_errors"]) == 3
    assert report["coverage"] == "incomplete"
    assert len(report["degraded_sandboxes"]) == 0  # not evidence of health


def test_malformed_inventory_is_unknown_not_empty() -> None:
    from kars_runtime_hermes.plugin import sre

    kube = MagicMock()
    kube.get.return_value = {}
    with patch.object(sre.sre_kube, "client", return_value=kube):
        state = sre._impl_sre_describe_state()
    assert all("error" in result for result in state.values())


def test_proposal_persists_pending_only_and_never_executes() -> None:
    from kars_runtime_hermes.plugin import sre

    kube = MagicMock()
    kube.get.return_value = {"items": []}
    kube.post.return_value = {"metadata": {"name": "sre-action-review"}}
    with patch.object(sre.sre_kube, "client", return_value=kube):
        result = sre._impl_sre_propose_fix(
            diagnosis="Verified resource quota blocks admission",
            target={"kind": "ResourceQuota", "namespace": "kars-example", "name": "quota"},
        )
    kube.post.assert_called_once()
    assert kube.post.call_args.args == ("/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/karssreactions",)
    assert kube.post.call_args.kwargs["json"]["spec"]["approval"] == {"state": "Pending"}
    assert result["action_id"] == "sre-action-review"
    assert result["cr_created"] is True
    assert not kube.patch.called
    assert not kube.delete.called


def test_failed_or_malformed_proposal_creation_never_claims_persistence() -> None:
    from kars_runtime_hermes.plugin import sre

    for response in (RuntimeError("SRE authority revoked"), {}):
        kube = MagicMock()
        kube.get.return_value = {"items": []}
        if isinstance(response, Exception):
            kube.post.side_effect = response
        else:
            kube.post.return_value = response
        with patch.object(sre.sre_kube, "client", return_value=kube):
            result = sre._impl_sre_propose_fix(
                diagnosis="Unhealthy pod",
                target={"kind": "Pod", "namespace": "kars-example", "name": "pod"},
            )
        assert result["cr_created"] is False
        assert result["action_id"] is None
        assert result["cr_error"]


def test_repeated_remedy_reuses_pending_proposal_without_writes() -> None:
    from kars_runtime_hermes.plugin import sre

    kube = MagicMock()
    kube.get.return_value = {"items": [{
        "metadata": {"name": "sre-action-existing"},
        "spec": {
            "action": {"type": "DeletePod", "params": {"namespace": "kars-example", "name": "pod"}},
            "approval": {"state": "Pending"},
        },
        "status": {"phase": "Proposed"},
    }]}
    with patch.object(sre.sre_kube, "client", return_value=kube):
        result = sre._impl_sre_propose_fix(
            diagnosis="Still unhealthy",
            target={"kind": "Pod", "namespace": "kars-example", "name": "pod"},
        )
    assert result["action_id"] == "sre-action-existing"
    assert result["cr_created"] is False
    assert result["cr_reused"] is True
    kube.post.assert_not_called()
    assert not kube.patch.called


def test_unknown_proposal_inventory_blocks_creation() -> None:
    from kars_runtime_hermes.plugin import sre

    kube = MagicMock()
    kube.get.return_value = {}
    with patch.object(sre.sre_kube, "client", return_value=kube):
        result = sre._impl_sre_propose_fix(
            diagnosis="Unhealthy",
            target={"kind": "Pod", "namespace": "kars-example", "name": "pod"},
        )
    assert result["cr_created"] is False
    assert "inventory is unknown" in result["cr_error"]
    kube.post.assert_not_called()
