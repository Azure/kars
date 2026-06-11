# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""kars-sre Slice 2 (K8s diagnostic toolset) tests."""

from __future__ import annotations

from unittest.mock import MagicMock, patch

import httpx


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


def test_what_changed_filters_to_failure_reasons() -> None:
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
