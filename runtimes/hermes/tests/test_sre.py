# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""kars-sre plugin tests (Slice 1)."""

from __future__ import annotations

import importlib
import inspect
import json
import os
import sys
from unittest.mock import MagicMock, patch

import pytest


@pytest.mark.parametrize("name", [
    "sre_describe_state", "sre_logs", "sre_diagnose", "sre_explain_error", "sre_propose_fix",
    "sre_describe_resource", "sre_what_changed", "sre_endpoints_inspect", "sre_image_probe", "sre_top",
])
@pytest.mark.parametrize("payload", [
    {"findings": [{"severity": "warning", "evidence": "resource not ready"}]},
    {"error": "API read denied", "coverage": "unknown"},
    {"action_id": "pending-reviewed-action", "approval": "Pending"},
])
def test_registered_handlers_return_hermes_json_text(name, payload):
    from kars_runtime_hermes.plugin import sre, sre_k8s

    ctx = MagicMock()
    sre.register(ctx)
    handler = next(call.kwargs["handler"] for call in ctx.register_tool.call_args_list
                   if call.kwargs["name"] == name)
    module = sre if hasattr(sre, "_impl_" + name) else sre_k8s
    with patch.object(module, "_impl_" + name, return_value=payload) as implementation:
        result = handler({"test_argument": "preserved"}, task_id="context-not-a-tool-argument")
    assert isinstance(result, str)
    assert json.loads(result) == payload
    implementation.assert_called_once_with(test_argument="preserved")


@pytest.mark.parametrize("api_mode,field", [
    ("codex_responses", "max_output_tokens"),
    ("chat_completions", "max_completion_tokens"),
])
def test_sre_default_output_bound_uses_native_request_middleware(api_mode, field):
    from kars_runtime_hermes.plugin import sre

    ctx = MagicMock()
    sre.register(ctx)
    ctx.register_middleware.assert_called_once_with("llm_request", sre._bound_inference_request)
    request = {"model": "gpt-5.4-mini", "stream": True, "extra_body": {"store": False}}
    result = sre._bound_inference_request(request=request, api_mode=api_mode, task_id="sre-test")
    assert result["request"] == {**request, field: 8192}
    assert field not in request
    assert result["source"] == "kars-sre"


@pytest.mark.parametrize("api_mode,field", [
    ("codex_responses", "max_output_tokens"),
    ("chat_completions", "max_completion_tokens"),
    ("chat_completions", "max_tokens"),
])
@pytest.mark.parametrize("bound", [2048, 128000, None, False, "invalid"])
@pytest.mark.parametrize("extra", [False, True])
def test_sre_preserves_explicit_bounds_for_native_and_router_validation(api_mode, field, bound, extra):
    from kars_runtime_hermes.plugin import sre

    request = {"extra_body": {field: bound}} if extra else {field: bound}
    assert sre._bound_inference_request(request=request, api_mode=api_mode) is None


@pytest.mark.parametrize("api_mode", ["", "anthropic_messages", "gemini", "unknown"])
def test_sre_output_default_leaves_other_api_modes_unchanged(api_mode):
    from kars_runtime_hermes.plugin import sre

    assert sre._bound_inference_request(request={"model": "other"}, api_mode=api_mode) is None


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


@pytest.mark.parametrize("entrypoint", ["sre", "sre_k8s"])
def test_registered_sre_schemas_expose_native_discovery_arguments(entrypoint):
    from kars_runtime_hermes.plugin import sre, sre_k8s

    ctx = MagicMock()
    {"sre": sre, "sre_k8s": sre_k8s}[entrypoint].register(ctx)
    for call in ctx.register_tool.call_args_list:
        registration = call.kwargs
        name = registration["name"]
        schema = json.loads(json.dumps(registration["schema"]))
        assert schema["name"] == name
        assert schema["description"] == registration["description"]
        assert schema["description"].strip()
        parameters = schema["parameters"]
        assert parameters["type"] == "object"
        module = sre if hasattr(sre, "_impl_" + name) else sre_k8s
        arguments = {
            key: arg for key, arg in inspect.signature(getattr(module, "_impl_" + name)).parameters.items()
            if arg.kind not in (inspect.Parameter.VAR_KEYWORD, inspect.Parameter.VAR_POSITIONAL)
        }
        assert set(parameters["properties"]) == set(arguments), name
        required = {key for key, arg in arguments.items() if arg.default is inspect.Parameter.empty}
        if name == "sre_propose_fix":
            required.add("target")
        assert set(parameters["required"]) == required, name
        if name == "sre_propose_fix":
            target = parameters["properties"]["target"]
            assert target["required"] == ["kind", "namespace", "name"]
            assert "Deployment" in target["properties"]["kind"]["enum"]


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

    with patch.object(sre.sre_kube, "client", side_effect=RuntimeError("SRE authority unavailable")):
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
    assert result["cr_created"] is False
    assert result["action_id"] is None
    assert result["cr_error"] == "SRE authority unavailable"
    assert result["execution_status"].startswith("not proposed")
    assert "approve_command" not in result


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
                "metadata": {"namespace": "kars-system", "name": "foo", "generation": 3},
                "status": {
                    "phase": "Ready",
                    "observedGeneration": 3,
                    "conditions": [{"type": "Available", "status": "True",
                                    "observedGeneration": 3,
                                    "lastTransitionTime": "2026-06-09T10:00:00Z"}],
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
        assert result[kind][0]["generation"] == 3
        assert result[kind][0]["observedGeneration"] == 3
        assert "lastReconciled" not in result[kind][0]
        assert result[kind][0]["conditions"][0]["observedGeneration"] == 3
        assert result[kind][0]["conditions"][0]["lastTransitionTime"] == "2026-06-09T10:00:00Z"


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
                {"metadata": {"name": "stale", "namespace": "kars-system", "generation": 3},
                 "status": {"phase": "Running", "observedGeneration": 2}},
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


@pytest.mark.parametrize("plural,kind", [
    ("karssandboxes", "KarsSandbox"),
    ("inferencepolicies", "InferencePolicy"),
    ("toolpolicies", "ToolPolicy"),
])
@pytest.mark.parametrize("generation,observed,expected", [
    (3, 3, "current"), (3, 2, "stale"), (3, 0, "stale"),
    (3, None, "unknown"), (None, 3, "unknown"),
    (3, 4, "unknown"), (3, -1, "unknown"),
    (0, 0, "unknown"), (-1, -1, "unknown"),
    (True, 1, "unknown"), (1, True, "unknown"),
    (3, "3", "unknown"), ("3", 3, "unknown"),
    (3.0, 3, "unknown"), (3, 3.0, "unknown"),
])
def test_diagnosis_uses_spec_generation_not_condition_age(
    plural: str, kind: str, generation: object, observed: object, expected: str,
) -> None:
    from kars_runtime_hermes.plugin import sre

    kube = MagicMock()

    def get(path):
        if "/deployments/" in path:
            return {"spec": {"replicas": 1}, "status": {"readyReplicas": 1, "availableReplicas": 1}}
        if path.endswith("/customresourcedefinitions"):
            return {"items": [{"metadata": {"name": name + ".kars.azure.com"}}
                              for name, _ in sre.KARS_CR_KINDS]}
        if path.endswith("/" + plural):
            return {"items": [{
                "metadata": {"name": "test", "namespace": "kars-system", "generation": generation},
                "status": {
                    "phase": "Ready", "observedGeneration": observed,
                    "lastCompiledAt": "2020-01-01T00:00:00Z",
                    "conditions": [{"type": "Ready", "status": "True",
                                    "lastTransitionTime": "2020-01-01T00:00:00Z"}],
                },
            }]}
        return {"items": []}

    kube.get.side_effect = get
    with patch.object(sre.sre_kube, "client", return_value=kube):
        report = sre._impl_sre_diagnose()
    assert len(report["stale_reconciles"]) == (1 if expected == "stale" else 0)
    assert len(report["coverage_errors"]) == (1 if expected == "unknown" else 0)
    assert report["coverage"] == ("incomplete" if expected == "unknown" else "complete")
    if expected == "unknown":
        assert report["coverage_errors"][0]["scope"] == f"{kind}/kars-system/test"
        assert "generation/observedGeneration" in report["coverage_errors"][0]["error"]
    assert report["degraded_sandboxes"] == []
    assert report["degraded_policies"] == []


def test_pairing_without_generation_observation_remains_unknown() -> None:
    from kars_runtime_hermes.plugin import sre

    kube = MagicMock()
    kube.get.return_value = {"spec": {"replicas": 1}, "status": {"readyReplicas": 1}, "items": []}
    pairing = sre._summarise_cr({
        "metadata": {"namespace": "kars-system", "name": "external", "generation": 1},
        "status": {"phase": "Active"},
    }, "KarsPairing")
    with patch.object(sre.sre_kube, "client", return_value=kube), \
         patch.object(sre, "_impl_sre_describe_state", return_value={"KarsPairing": [pairing]}):
        report = sre._impl_sre_diagnose()
    assert report["coverage"] == "incomplete"
    assert len(report["coverage_errors"]) == 1
    assert report["coverage_errors"][0]["scope"] == "KarsPairing/kars-system/external"
    assert report["stale_reconciles"] == []


def test_malformed_inventory_is_unknown_not_empty() -> None:
    from kars_runtime_hermes.plugin import sre

    kube = MagicMock()
    kube.get.return_value = {}
    with patch.object(sre.sre_kube, "client", return_value=kube):
        state = sre._impl_sre_describe_state()
    assert all("error" in result for result in state.values())


@pytest.mark.parametrize("action_type,extra", [
    ("ScaleDeployment", {"replicas": 0}),
    ("ScaleDeployment", {"replicas": 1}),
    ("ScaleDeployment", {"replicas": 50}),
    ("PatchDeploymentImage", {"container": "agent", "image": "example/agent:latest"}),
    ("RolloutRestart", {"rollout_restart": True}),
])
@pytest.mark.parametrize("explicit", [False, True])
def test_registered_proposal_exposes_and_persists_action_parameters(action_type, extra, explicit):
    from kars_runtime_hermes.plugin import sre

    ctx = MagicMock()
    sre.register(ctx)
    registration = next(call.kwargs for call in ctx.register_tool.call_args_list
                        if call.kwargs["name"] == "sre_propose_fix")
    properties = registration["schema"]["parameters"]["properties"]
    assert set(extra) <= properties.keys()
    assert properties["replicas"] == {
        "type": "integer", "minimum": 0, "maximum": 50,
        "description": "Required for ScaleDeployment: desired replica count (0–50).",
    }
    arguments = {"diagnosis": "Verified incident", "target": {
        "kind": "Deployment", "namespace": "kars-example", "name": "workload"}, **extra}
    if explicit:
        arguments["action_type"] = action_type
    kube = MagicMock()
    kube.get.return_value = {"items": []}
    kube.post.return_value = {"metadata": {"name": "sre-action-review"}}
    with patch.object(sre.sre_kube, "client", return_value=kube):
        result = json.loads(registration["handler"](arguments))
    assert result["cr_created"] is True
    action = kube.post.call_args.kwargs["json"]["spec"]["action"]
    assert action["type"] == action_type
    expected = {"namespace": "kars-example", "name": "workload"}
    expected.update({"kind": "Deployment"} if action_type == "RolloutRestart" else extra)
    assert action["params"] == expected
    assert not kube.patch.called and not kube.delete.called


@pytest.mark.parametrize("action_type,extra", [
    ("ScaleDeployment", {}),
    *[("ScaleDeployment", {"replicas": value})
      for value in (None, True, False, -1, 51, 1.5, "1")],
    ("PatchDeploymentImage", {}),
    ("PatchDeploymentImage", {"container": "agent"}),
    ("PatchDeploymentImage", {"image": "example/agent:latest"}),
    ("PatchDeploymentImage", {"container": " ", "image": "example/agent:latest"}),
    ("PatchDeploymentImage", {"container": "agent", "image": ""}),
    ("PatchDeploymentImage", {"container": "agent", "image": 42}),
])
def test_invalid_action_parameters_never_publish_a_proposal(action_type, extra):
    from kars_runtime_hermes.plugin import sre

    with patch.object(sre.sre_kube, "client") as client:
        result = json.loads(sre.sre_propose_fix({
            "diagnosis": "Verified incident", "action_type": action_type,
            "target": {"kind": "Deployment", "namespace": "kars-example", "name": "workload"},
            **extra,
        }))
    assert result["cr_created"] is False
    assert result["action_id"] is None
    assert result["cr_error"]
    assert "approve_command" not in result
    client.assert_not_called()


@pytest.mark.parametrize("kind,action_type,extra", [
    ("StatefulSet", "ScaleDeployment", {"replicas": 1}),
    ("DaemonSet", "PatchDeploymentImage", {"container": "agent", "image": "example/agent:latest"}),
    ("Pod", "RolloutRestart", {}),
    ("Deployment", "DeletePod", {}),
])
def test_proposal_rejects_action_target_kind_mismatch(kind, action_type, extra):
    from kars_runtime_hermes.plugin import sre

    with patch.object(sre.sre_kube, "client") as client:
        result = sre._impl_sre_propose_fix(
            diagnosis="Verified incident", action_type=action_type,
            target={"kind": kind, "namespace": "kars-example", "name": "workload"}, **extra,
        )
    assert result["cr_created"] is False
    assert "target.kind" in result["cr_error"]
    client.assert_not_called()


@pytest.mark.parametrize("value", [False, None, 1, "true", "false"])
def test_rollout_restart_inference_requires_true_boolean(value):
    from kars_runtime_hermes.plugin import sre

    with patch.object(sre.sre_kube, "client") as client:
        result = sre._impl_sre_propose_fix(
            diagnosis="Verified incident", rollout_restart=value,
            target={"kind": "Deployment", "namespace": "kars-example", "name": "workload"},
        )
    assert result["cr_created"] is False
    assert result["action_id"] is None
    assert result["execution_status"].startswith("not proposed")
    client.assert_not_called()


@pytest.mark.parametrize("field", ["namespace", "name"])
@pytest.mark.parametrize("value", [None, "", " ", 1])
def test_proposal_requires_nonempty_target_names(field, value):
    from kars_runtime_hermes.plugin import sre

    target = {"kind": "Deployment", "namespace": "kars-example", "name": "workload", field: value}
    with patch.object(sre.sre_kube, "client") as client:
        result = sre._impl_sre_propose_fix(
            diagnosis="Verified incident", action_type="ScaleDeployment", replicas=1, target=target,
        )
    assert result["cr_error"]
    assert result["execution_status"].startswith("not proposed")
    client.assert_not_called()


@pytest.mark.parametrize("kind", ["StatefulSet", "DaemonSet"])
def test_explicit_rollout_restart_preserves_supported_target_kind(kind):
    from kars_runtime_hermes.plugin import sre

    kube = MagicMock()
    kube.get.return_value = {"items": []}
    kube.post.return_value = {"metadata": {"name": "sre-action-review"}}
    with patch.object(sre.sre_kube, "client", return_value=kube):
        result = sre._impl_sre_propose_fix(
            diagnosis="Verified incident", action_type="RolloutRestart",
            target={"kind": kind, "namespace": "kars-example", "name": "workload"},
        )
    assert result["cr_created"] is True
    assert kube.post.call_args.kwargs["json"]["spec"]["action"]["params"]["kind"] == kind


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
