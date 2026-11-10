# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""kars-sre Slice 2 (K8s diagnostic toolset) tests."""

from __future__ import annotations

from datetime import datetime, timezone
from unittest.mock import MagicMock, patch

import httpx
import pytest


def test_register_registers_five_slice2_tools() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    ctx = MagicMock()
    sre_k8s.register(ctx)
    tool_names = {call.kwargs["name"] for call in ctx.register_tool.call_args_list}
    assert tool_names == {
        "sre_describe_resource",
        "sre_what_changed",
        "sre_endpoints_inspect",
        "sre_image_probe",
        "sre_top",
    }


def test_describe_resource_unknown_kind() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    result = sre_k8s._impl_sre_describe_resource(kind="UnknownKind", name="x")
    assert "error" in result
    assert "supported_kinds" in result


def test_describe_resource_karseval() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    condition = {
        "type": "Degraded", "status": "True", "reason": "Inconclusive",
        "message": "One case could not be exercised", "observedGeneration": 3,
        "lastTransitionTime": "2026-11-04T08:00:00Z",
    }
    result_summary = {
        "schemaVersion": "1", "corpusDigest": "sha256:corpus",
        "total": 3, "passed": 2, "failed": 0, "errored": 1,
        "corpusLabel": "jailbreak-baseline", "jobName": "eval-job",
        "firstFailingCases": [],
    }
    eval_doc = {
        "metadata": {
            "namespace": "kars-system", "name": "check", "generation": 3,
            "annotations": {"private": "do-not-return"},
        },
        "spec": {
            "targetSandboxRef": {"name": "research", "private": "do-not-return"},
            "corpus": {"builtin": "jailbreak-baseline", "private": "do-not-return"},
            "schedule": "0 * * * *", "failSandboxOnDrift": False,
            "private": "do-not-return",
        },
        "status": {
            "phase": "Degraded", "observedGeneration": 3, "conditions": [condition],
            "lastRunAt": "2026-11-04T08:00:00Z",
            "lastResult": {**result_summary, "private": "do-not-return"},
            "reportConfigMapRef": {"name": "eval-report", "private": "do-not-return"},
            "reportConfigMapUid": "report-uid", "reportEvidenceDigest": "sha256:evidence",
            "private": "do-not-return",
        },
    }
    mock_client = MagicMock()
    mock_client.get.side_effect = [eval_doc, {"items": []}]
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_describe_resource(
            kind="KarsEval", namespace="kars-system", name="check",
        )
    assert mock_client.get.call_args_list[0].args == (
        "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karsevals/check",
    )
    assert mock_client.get.call_args_list[1].kwargs["params"] == {
        "fieldSelector": (
            "involvedObject.kind=KarsEval,involvedObject.name=check,"
            "involvedObject.namespace=kars-system"
        ),
        "limit": 25,
    }
    assert result["kind"] == "KarsEval"
    assert result["phase"] == "Degraded"
    assert result["conditions"] == [condition]
    assert result["generation"] == result["observedGeneration"] == 3
    assert result["targetSandboxRef"] == {"name": "research"}
    assert result["schedule"] == "0 * * * *"
    assert result["failSandboxOnDrift"] is False
    assert result["lastResult"] == result_summary
    assert result["lastRunAt"] == "2026-11-04T08:00:00Z"
    assert result["reportConfigMapRef"] == {"name": "eval-report"}
    assert result["reportConfigMapUid"] == "report-uid"
    assert result["reportEvidenceDigest"] == "sha256:evidence"
    assert result["recent_events"] == []
    assert "do-not-return" not in str(result)


def test_describe_resource_karseval_requires_namespace() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    with patch.object(sre_k8s.sre_kube, "client") as client:
        result = sre_k8s._impl_sre_describe_resource(kind="KarsEval", name="check")
    assert result == {"error": "KarsEval is namespaced — provide namespace"}
    client.assert_not_called()


@pytest.mark.parametrize("status", [{}, {"phase": "Pending", "lastResult": None}])
def test_describe_resource_karseval_no_result_is_not_a_pass(status) -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    mock_client = MagicMock()
    mock_client.get.side_effect = [
        {"metadata": {"name": "check", "namespace": "kars-system"}, "status": status},
        {"items": []},
    ]
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_describe_resource(
            kind="KarsEval", namespace="kars-system", name="check",
        )
    assert result["phase"] == status.get("phase")
    assert result["lastResult"] is None
    assert result["lastRunAt"] is None


@pytest.mark.parametrize("code,reason", [(403, "Forbidden"), (404, "Not Found")])
def test_describe_resource_karseval_api_failure(code, reason) -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    mock_client = MagicMock()
    response = httpx.Response(code, request=httpx.Request("GET", "https://localhost/"))
    mock_client.get.side_effect = httpx.HTTPStatusError(
        reason, request=response.request, response=response,
    )
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_describe_resource(
            kind="KarsEval", namespace="kars-system", name="check",
        )
    assert result["error"] == f"{code} {reason}"
    assert result["kind"] == "KarsEval"
    mock_client.get.assert_called_once()


def test_describe_resource_egress_approval() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    condition = {
        "type": "Ready", "status": "True", "reason": "RouterConfirmed",
        "message": "Router reported the merged digest", "observedGeneration": 2,
        "lastTransitionTime": "2026-11-04T08:00:00Z",
    }
    status = {
        "phase": "Active", "observedGeneration": 2, "conditions": [condition],
        "effectiveAt": "2026-11-04T08:00:00Z", "expiresAt": "2026-11-04T08:15:00Z",
        "mergedDigest": "sha256:merged", "hostCount": 1, "usageCount": 0,
    }
    client = MagicMock()
    client.get.side_effect = [{
        "metadata": {
            "name": "temporary", "namespace": "kars-system", "generation": 3,
            "annotations": {"private": "do-not-return"},
        },
        "spec": {
            "sandbox": "research", "ttl": "PT15M",
            "hosts": [{"host": "example.com", "port": 443, "private": "do-not-return"}],
            "reason": "do-not-return", "ticket": "do-not-return",
        },
        "status": {**status, "private": "do-not-return"},
    }, {"items": []}]
    with patch.object(sre_k8s.sre_kube, "client", return_value=client):
        result = sre_k8s._impl_sre_describe_resource(
            kind="EgressApproval", namespace="kars-system", name="temporary",
        )
    assert client.get.call_args_list[0].args == (
        "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/egressapprovals/temporary",
    )
    assert result == {
        "kind": "EgressApproval", "namespace": "kars-system", "name": "temporary",
        "generation": 3, "sandbox": "research", "ttl": "PT15M",
        "hosts": [{"host": "example.com", "port": 443}], "recent_events": [], **status,
    }
    assert client.get.call_args_list[1].kwargs["params"] == {
        "fieldSelector": (
            "involvedObject.kind=EgressApproval,involvedObject.name=temporary,"
            "involvedObject.namespace=kars-system"
        ), "limit": 25,
    }
    assert "do-not-return" not in str(result)


@pytest.mark.parametrize("status", [None, {}, {"conditions": None, "phase": "Pending"}])
@pytest.mark.parametrize("kind", ["KarsEval", "EgressApproval"])
def test_describe_resource_cr_missing_observation(kind, status) -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    client = MagicMock()
    client.get.side_effect = [
        {"metadata": {"name": "check", "namespace": "kars-system"}, "status": status},
        {"items": []},
    ]
    with patch.object(sre_k8s.sre_kube, "client", return_value=client):
        result = sre_k8s._impl_sre_describe_resource(kind=kind, namespace="kars-system", name="check")
    assert "error" not in result
    assert result["conditions"] == []
    assert result["observedGeneration"] is None
    if kind == "EgressApproval":
        assert result["expiresAt"] is None
        assert result["usageCount"] is None
        assert result["hostCount"] is None
        assert result["hosts"] is None


def test_describe_resource_egress_approval_requires_namespace() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    with patch.object(sre_k8s.sre_kube, "client") as client:
        result = sre_k8s._impl_sre_describe_resource(kind="EgressApproval", name="check")
    assert result == {"error": "EgressApproval is namespaced — provide namespace"}
    client.assert_not_called()


@pytest.mark.parametrize("code,reason", [(403, "Forbidden"), (404, "Not Found")])
def test_describe_resource_egress_approval_api_failure(code, reason) -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    client = MagicMock()
    response = httpx.Response(code, request=httpx.Request("GET", "https://localhost/"))
    client.get.side_effect = httpx.HTTPStatusError(reason, request=response.request, response=response)
    with patch.object(sre_k8s.sre_kube, "client", return_value=client):
        result = sre_k8s._impl_sre_describe_resource(
            kind="EgressApproval", namespace="kars-system", name="check",
        )
    assert result["error"] == f"{code} {reason}"
    assert result["kind"] == "EgressApproval"
    client.get.assert_called_once()


def test_describe_resource_resource_quota() -> None:
    """ResourceQuota describe surfaces the kars-managed label."""
    from kars_runtime_hermes.plugin import sre_k8s

    quota_doc = {
        "metadata": {
            "namespace": "kars-research",
            "name": "platform-hardening-quota",
            "labels": {
                "app.kubernetes.io/managed-by": "gitops-platform",
            },
        },
        "spec": {"hard": {"requests.memory": "50Mi"}},
        "status": {"used": {"requests.memory": "0"}},
    }
    mock_client = MagicMock()
    mock_client.get.side_effect = [quota_doc, {"items": []}]  # quota + events
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_describe_resource(
            kind="ResourceQuota",
            namespace="kars-research",
            name="platform-hardening-quota",
        )
    assert result["kind"] == "ResourceQuota"
    assert result["name"] == "platform-hardening-quota"
    assert result["hard"] == {"requests.memory": "50Mi"}
    # Crucially, the SRE agent must be able to tell this is NOT
    # kars-managed (label doesn't have managed-by=controller) — so
    # DeleteResourceQuota is permitted on this resource.
    assert result["isKarsManaged"] is False


def test_describe_resource_resource_quota_kars_managed() -> None:
    """ResourceQuota labelled as kars-managed surfaces isKarsManaged=True."""
    from kars_runtime_hermes.plugin import sre_k8s

    quota_doc = {
        "metadata": {
            "namespace": "kars-sre",
            "name": "sre-quota",
            "labels": {"kars.azure.com/managed-by": "controller"},
        },
        "spec": {"hard": {"requests.memory": "1Gi"}},
        "status": {},
    }
    mock_client = MagicMock()
    mock_client.get.side_effect = [quota_doc, {"items": []}]
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_describe_resource(
            kind="ResourceQuota", namespace="kars-sre", name="sre-quota"
        )
    assert result["isKarsManaged"] is True


def test_describe_resource_deployment_owner_graph() -> None:
    """A Deployment describe walks workload → RS → Pods → events."""
    from kars_runtime_hermes.plugin import sre_k8s

    deploy_doc = {
        "kind": "Deployment",
        "metadata": {"namespace": "kars-research", "name": "research", "generation": 1},
        "spec": {
            "selector": {"matchLabels": {"app": "research"}},
            "template": {
                "spec": {
                    "containers": [{"name": "openclaw", "image": "kars/hermes:latest"}]
                }
            },
        },
        "status": {"replicas": 1, "readyReplicas": 0, "availableReplicas": 0},
    }
    rs_doc = {
        "items": [
            {
                "kind": "ReplicaSet",
                "metadata": {"namespace": "kars-research", "name": "research-abc123"},
                "spec": {"selector": {"matchLabels": {"app": "research"}}},
                "status": {"replicas": 1, "readyReplicas": 0},
            }
        ]
    }
    pod_doc = {
        "items": [
            {
                "metadata": {"namespace": "kars-research", "name": "research-abc123-xyz"},
                "spec": {"nodeName": None},
                "status": {
                    "phase": "Pending",
                    "containerStatuses": [],
                    "conditions": [],
                },
            }
        ]
    }
    mock_client = MagicMock()
    # Workload, RS list, Pod list, then per-object events (3 calls — one for
    # the Deployment, one for the RS, one for the Pod)
    mock_client.get.side_effect = [
        deploy_doc, rs_doc, pod_doc,
        {"items": []}, {"items": []}, {"items": []},
    ]
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_describe_resource(
            kind="Deployment", namespace="kars-research", name="research"
        )
    assert "workload" in result
    assert result["workload"]["name"] == "research"
    assert "pods" in result
    assert isinstance(result["pods"], list)
    assert len(result["pods"]) == 1
    assert result["pods"][0]["phase"] == "Pending"


def test_describe_resource_handles_404_gracefully() -> None:
    """A 404 on the workload doesn't raise — surfaces as {error: ...}."""
    from kars_runtime_hermes.plugin import sre_k8s

    mock_client = MagicMock()
    response = MagicMock(status_code=404, reason_phrase="Not Found")
    mock_client.get.side_effect = httpx.HTTPStatusError("404", request=MagicMock(), response=response)
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_describe_resource(
            kind="Pod", namespace="kars-research", name="missing"
        )
    assert "error" in result
    assert "404" in result["error"]


@pytest.fixture
def event_clock():
    from kars_runtime_hermes.plugin import sre_k8s

    with patch.object(sre_k8s, "datetime", wraps=datetime) as clock:
        clock.now.return_value = datetime(2026, 6, 9, 11, 0, tzinfo=timezone.utc)
        yield clock


def test_what_changed_filters_to_failure_reasons(event_clock) -> None:
    """Only events with reasons in WHAT_CHANGED_REASONS surface."""
    from kars_runtime_hermes.plugin import sre_k8s

    core_doc = {
        "items": [
            {
                "involvedObject": {"kind": "ReplicaSet", "namespace": "kars-research", "name": "research-abc"},
                "type": "Warning",
                "reason": "FailedCreate",
                "message": "pods is forbidden: exceeded quota",
                "count": 5,
                "lastTimestamp": "2026-06-09T10:50:00Z",
            },
            {
                "involvedObject": {"kind": "Pod", "namespace": "kars-research", "name": "research-xyz"},
                "type": "Normal",
                "reason": "Scheduled",   # NOT in WHAT_CHANGED_REASONS — should be filtered out
                "message": "Successfully assigned",
            },
        ]
    }
    new_doc = {"items": []}
    mock_client = MagicMock()
    mock_client.get.side_effect = [core_doc, new_doc]
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_what_changed(namespace="kars-research", minutes=15)
    assert len(result["events_core"]) == 1
    assert result["events_core"][0]["reason"] == "FailedCreate"
    assert "exceeded quota" in result["events_core"][0]["message"]


@pytest.mark.parametrize("api", ["core", "new"])
@pytest.mark.parametrize(
    "timestamps,included,undated",
    [
        ({"lastTimestamp": "2026-06-09T10:45:00Z"}, True, 0),
        ({"lastTimestamp": "2026-06-09T11:00:00Z"}, True, 0),
        ({"lastTimestamp": "2026-06-09T10:44:59.999999Z"}, False, 0),
        ({"lastTimestamp": "2026-06-09T11:00:00.000001Z"}, False, 0),
        ({"eventTime": "2026-06-09T12:50:00+02:00"}, True, 0),
        ({"eventTime": None, "deprecatedLastTimestamp": "2026-06-09T10:50:00Z"}, True, 0),
        ({"eventTime": "2026-06-09T10:00:00Z",
          "series": {"lastObservedTime": "2026-06-09T10:55:00Z"}}, True, 0),
        ({"eventTime": "2026-06-09T10:00:00Z",
          "series": {"lastObservedTime": "2026-06-09T10:40:00Z"}}, False, 0),
        ({"firstTimestamp": "2026-06-09T10:50:00Z"}, True, 0),
        ({"deprecatedFirstTimestamp": "2026-06-09T10:50:00Z"}, True, 0),
        ({"eventTime": "invalid", "lastTimestamp": "2026-06-09T10:50:00Z"}, True, 0),
        ({"lastTimestamp": "2026-06-09T10:50:00"}, False, 1),
        ({"eventTime": "invalid"}, False, 1),
        ({"metadata": {"creationTimestamp": "2026-06-09T10:50:00Z"}}, False, 1),
        ({}, False, 1),
    ],
)
def test_what_changed_enforces_observation_window(event_clock, api, timestamps, included, undated):
    from kars_runtime_hermes.plugin import sre_k8s

    event = {"reason": "Unhealthy", **timestamps}
    docs = [{"items": [event]}, {"items": []}]
    if api == "new":
        docs.reverse()
    client = MagicMock()
    client.get.side_effect = docs
    with patch.object(sre_k8s.sre_kube, "client", return_value=client):
        result = sre_k8s._impl_sre_what_changed()
    key = f"events_{api}"
    assert len(result[key]) == int(included)
    assert result["excluded_undated"][key] == undated
    assert result["as_of"] == "2026-06-09T11:00:00+00:00"
    assert result["since"] == "2026-06-09T10:45:00+00:00"
    if included:
        observed = datetime.fromisoformat(result[key][0]["lastObservedTime"])
        assert datetime.fromisoformat(result["since"]) <= observed <= datetime.fromisoformat(result["as_of"])


@pytest.mark.parametrize("api", ["core", "new"])
def test_what_changed_follows_all_event_pages(event_clock, api):
    from kars_runtime_hermes.plugin import sre_k8s

    pages = [
        {"items": [{"reason": "Unhealthy", "lastTimestamp": "2026-06-09T10:00:00Z"}],
         "metadata": {"continue": "next-page"}},
        {"items": [{"reason": "Unhealthy", "lastTimestamp": "2026-06-09T10:50:00Z"}]},
    ]
    empty = {"items": []}
    client = MagicMock()
    client.get.side_effect = [*pages, empty] if api == "core" else [empty, *pages]
    with patch.object(sre_k8s.sre_kube, "client", return_value=client):
        result = sre_k8s._impl_sre_what_changed(namespace="kars-sre")
    assert len(result[f"events_{api}"]) == 1
    calls = client.get.call_args_list
    page_call = calls[1 if api == "core" else 2]
    assert page_call.kwargs["params"] == {"limit": 200, "continue": "next-page"}
    assert "/namespaces/kars-sre/events" in page_call.args[0]


@pytest.mark.parametrize("broken", [{}, {"items": None}, {"items": {}}])
def test_what_changed_invalid_inventory_is_error_not_healthy(event_clock, broken):
    from kars_runtime_hermes.plugin import sre_k8s

    client = MagicMock()
    client.get.side_effect = [broken, {"items": []}]
    with patch.object(sre_k8s.sre_kube, "client", return_value=client):
        result = sre_k8s._impl_sre_what_changed()
    assert "error" in result["events_core"]
    assert result["events_new"] == []


def test_what_changed_repeated_page_token_is_error(event_clock):
    from kars_runtime_hermes.plugin import sre_k8s

    page = {"items": [], "metadata": {"continue": "repeated"}}
    client = MagicMock()
    client.get.side_effect = [page, page, {"items": []}]
    with patch.object(sre_k8s.sre_kube, "client", return_value=client):
        result = sre_k8s._impl_sre_what_changed()
    assert "repeated continuation token" in result["events_core"]["error"]
    assert client.get.call_count == 3


@pytest.mark.parametrize("requested,minutes", [(-2, 1), (0, 1), (60, 60), (90, 60)])
def test_what_changed_clamps_lookback(event_clock, requested, minutes):
    from kars_runtime_hermes.plugin import sre_k8s

    client = MagicMock()
    client.get.return_value = {"items": []}
    with patch.object(sre_k8s.sre_kube, "client", return_value=client):
        result = sre_k8s._impl_sre_what_changed(minutes=requested)
    assert result["since_minutes"] == minutes
    duration = datetime.fromisoformat(result["as_of"]) - datetime.fromisoformat(result["since"])
    assert duration.total_seconds() == minutes * 60


def test_endpoints_inspect_zero_endpoints_finding() -> None:
    """Service with pods that are NotReady → finding describes the issue."""
    from kars_runtime_hermes.plugin import sre_k8s

    svc_doc = {
        "spec": {"selector": {"app": "research"}, "type": "ClusterIP"},
    }
    pod_doc = {
        "items": [
            {
                "metadata": {"name": "research-1"},
                "status": {
                    "phase": "Running",
                    "podIP": "10.244.0.5",
                    "conditions": [{"type": "Ready", "status": "False"}],
                },
            },
            {
                "metadata": {"name": "research-2"},
                "status": {
                    "phase": "Running",
                    "podIP": "10.244.0.6",
                    "conditions": [{"type": "Ready", "status": "False"}],
                },
            },
        ]
    }
    es_doc = {"items": []}
    mock_client = MagicMock()
    mock_client.get.side_effect = [svc_doc, pod_doc, es_doc]
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_endpoints_inspect(namespace="kars-research", service="research")
    assert result["selector"] == {"app": "research"}
    assert len(result["matching_pods"]) == 2
    # Both pods are NotReady → finding should call that out
    assert "none are Ready" in result["finding"]


def test_endpoints_inspect_pod_selector_mismatch() -> None:
    """Service whose selector matches no pods → clear finding."""
    from kars_runtime_hermes.plugin import sre_k8s

    svc_doc = {"spec": {"selector": {"app": "wrong-name"}, "type": "ClusterIP"}}
    pod_doc = {"items": []}
    es_doc = {"items": []}
    mock_client = MagicMock()
    mock_client.get.side_effect = [svc_doc, pod_doc, es_doc]
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_endpoints_inspect(namespace="kars-research", service="research")
    assert "No pods match" in result["finding"]


@pytest.mark.parametrize("slice_fields", [{}, {"endpoints": None}, {"endpoints": []}])
def test_empty_endpoint_slice_is_a_valid_outage(slice_fields) -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    kube = MagicMock()
    kube.get.side_effect = [
        {"spec": {"selector": {"app": "recovery-check"}}},
        {"items": []},
        {"items": [{"metadata": {"name": "empty"}, **slice_fields}]},
    ]
    with patch.object(sre_k8s.sre_kube, "client", return_value=kube):
        result = sre_k8s._impl_sre_endpoints_inspect(namespace="example", service="recovery-check")
    assert result["endpoint_slices"][0]["endpoints"] == []
    assert result.get("coverage") != "incomplete"
    assert "No pods match" in result["finding"]


@pytest.mark.parametrize("endpoints", [{}, "", 0])
def test_malformed_endpoint_slice_remains_unknown(endpoints) -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    kube = MagicMock()
    kube.get.side_effect = [
        {"spec": {"selector": {"app": "recovery-check"}}},
        {"items": []}, {"items": [{"endpoints": endpoints}]},
    ]
    with patch.object(sre_k8s.sre_kube, "client", return_value=kube):
        result = sre_k8s._impl_sre_endpoints_inspect(namespace="example", service="recovery-check")
    assert result["coverage"] == "incomplete"
    assert "unknown" in result["finding"]


@pytest.mark.parametrize("desired,observed", [(0, None), (3, 1)])
def test_workload_separates_desired_and_observed_replicas(desired, observed) -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    result = sre_k8s._summarise_workload({"spec": {"replicas": desired}, "status": {"replicas": observed}})
    assert result["desiredReplicas"] == desired
    assert result["replicas"] == observed


def test_image_probe_parses_canonical_image_string() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    parsed = sre_k8s._parse_image("docker.io/nginx:1.27.3")
    assert parsed["registry"] == "docker.io"
    assert parsed["repo"] == "nginx"
    assert parsed["tag"] == "1.27.3"

    parsed = sre_k8s._parse_image("nginx:1.27-typo")
    assert parsed["repo"] == "nginx"
    assert parsed["tag"] == "1.27-typo"


def test_image_probe_finds_closest_tag_in_use() -> None:
    """When the requested image isn't in use but a similar one is, suggest it."""
    from kars_runtime_hermes.plugin import sre_k8s

    pod_doc = {
        "items": [
            {"spec": {"containers": [{"image": "nginx:1.27.3"}], "initContainers": []}},
            {"spec": {"containers": [{"image": "nginx:1.27.3"}], "initContainers": []}},
            {"spec": {"containers": [{"image": "redis:7"}], "initContainers": []}},
        ]
    }
    mock_client = MagicMock()
    mock_client.get.return_value = pod_doc
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_image_probe(image="nginx:1.27-typo")
    # The closest in-use match for nginx:1.27-typo is nginx:1.27.3
    assert result["closest_in_use"] == "nginx:1.27.3"
    assert "typo" in result["advice"].lower() or "edit-distance" in result["advice"]
    assert len(result["in_use_on_cluster"]) >= 1


def test_image_probe_no_pods_use_repo() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    pod_doc = {"items": []}
    mock_client = MagicMock()
    mock_client.get.return_value = pod_doc
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_image_probe(image="newrepo:v1")
    assert result["in_use_on_cluster"] == []
    assert "No pod on this cluster" in result["advice"]


def test_top_unavailable_when_metrics_server_missing() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    mock_client = MagicMock()
    response = MagicMock(status_code=404, reason_phrase="Not Found")
    mock_client.get.side_effect = httpx.HTTPStatusError(
        "404", request=MagicMock(), response=response
    )
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_top(scope="nodes")
    assert "unavailable" in result
    assert "metrics-server" in result["unavailable"]


def test_top_invalid_scope() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    result = sre_k8s._impl_sre_top(scope="invalid")
    assert "error" in result
    assert "valid_scopes" in result


def test_top_pods_returns_per_container() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    doc = {
        "items": [
            {
                "metadata": {"namespace": "kars-research", "name": "research-pod"},
                "timestamp": "2026-06-09T10:55:00Z",
                "containers": [
                    {"name": "openclaw", "usage": {"cpu": "5m", "memory": "120Mi"}},
                    {"name": "inference-router", "usage": {"cpu": "1m", "memory": "20Mi"}},
                ],
            }
        ]
    }
    mock_client = MagicMock()
    mock_client.get.return_value = doc
    with patch.object(sre_k8s.sre_kube, "client", return_value=mock_client):
        result = sre_k8s._impl_sre_top(scope="pods", namespace="kars-research")
    assert result["scope"] == "pods"
    assert len(result["items"]) == 1
    assert len(result["items"][0]["containers"]) == 2


def test_edit_distance() -> None:
    """Sanity-check the Levenshtein implementation underlying image_probe."""
    from kars_runtime_hermes.plugin import sre_k8s

    assert sre_k8s._edit_distance("", "") == 0
    assert sre_k8s._edit_distance("abc", "abc") == 0
    assert sre_k8s._edit_distance("abc", "abd") == 1
    assert sre_k8s._edit_distance("1.27.3", "1.27-typo") <= 5


def test_events_failure_is_not_an_empty_event_history() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    kube = MagicMock()
    kube.get.side_effect = RuntimeError("Forbidden")
    result = sre_k8s._events_for_object(kube, "kars-example", "Pod", "example")
    assert result == [{"error": "Forbidden", "namespace": "kars-example", "kind": "Pod", "name": "example"}]


def test_service_inventory_failure_never_claims_selector_mismatch() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    kube = MagicMock()
    kube.get.side_effect = [
        {"metadata": {"name": "service"}, "spec": {"selector": {"app": "example"}}},
        RuntimeError("Forbidden"), {"items": []},
    ]
    with patch.object(sre_k8s.sre_kube, "client", return_value=kube):
        result = sre_k8s._impl_sre_endpoints_inspect(namespace="kars-example", service="service")
    assert result["coverage"] == "incomplete"
    assert "unknown" in result["finding"]
    assert result["matching_pods"] == {"error": "Forbidden"}


def test_pod_without_ready_condition_is_not_ready() -> None:
    from kars_runtime_hermes.plugin import sre_k8s

    kube = MagicMock()
    kube.get.side_effect = [
        {"metadata": {"name": "service"}, "spec": {"selector": {"app": "example"}}},
        {"items": [{"metadata": {"name": "pod"}, "status": {"phase": "Running"}}]},
        {"items": []},
    ]
    with patch.object(sre_k8s.sre_kube, "client", return_value=kube):
        result = sre_k8s._impl_sre_endpoints_inspect(namespace="kars-example", service="service")
    assert result["matching_pods"][0]["ready"] is False
    assert "none are Ready" in result["finding"]
