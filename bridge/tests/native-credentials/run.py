"""Hosted-only native acceptance. Each emitted marker records an actual outcome."""

import json
import os
import sys
import time
from datetime import datetime, timezone

from api_gate import CORE_REVISION
from api_outcome_diagnostics import collect as api_outcome_diagnostics
from boot import bridge_connection, install_bridge, install_core
from credential_cases import CredentialCases
from credential_diagnostics import condition_category, runtime_scope
from lifecycle_cases import LifecycleCases
from native_api import CORE, STATE, Failure, Setup, command, core, require, scheduling_detail, status_detail
from observation_cases import ObservationCases
from observation_diagnostics import collect as observation_diagnostics
from observer_network_diagnostics import collect as observer_network_diagnostics
from template_diagnostics import collect as template_diagnostics


def diagnostics(setup):
    result = {}
    for label, path in [
        ("pods", "/api/v1/pods"),
        ("grants", "/apis/kars.azure.com/v1alpha1/karscredentialgrants"),
        ("tasks", "/apis/kars.azure.com/v1alpha1/karstasks"),
        ("sandboxes", "/apis/kars.azure.com/v1alpha1/karssandboxes"),
        ("deployments", "/apis/apps/v1/deployments"),
    ]:
        try:
            objects = setup.admin.get(path)["items"]
            result[label] = [{
                "namespace": item["metadata"].get("namespace"),
                "name": item["metadata"]["name"], "uid": item["metadata"]["uid"],
                "phase": item.get("status", {}).get("phase"),
                "generation": item["metadata"].get("generation"),
                "observedGeneration": item.get("status", {}).get("observedGeneration"),
                "executionPhase": item.get("status", {}).get("executionPhase"),
                "executionDetail": item.get("status", {}).get("executionDetail"),
                "envelopeDigest": item.get("status", {}).get("envelopeDigest"),
                "credentialMarkers": {key: value for key, value in item["metadata"].get("annotations", {}).items()
                                      if key in ["kars.azure.com/credential-rebind-pending",
                                                 "kars.azure.com/credential-rebind-task-uid",
                                                 "kars.azure.com/credential-bundle-uid",
                                                 "kars.azure.com/namespace-uid"]},
                "credentialBindings": item.get("spec", {}).get("credentialBindings",
                    (item.get("spec", {}).get("blueprint") or {}).get("credentialBindings")),
                "credentialsRef": item.get("spec", {}).get("credentialsRef"),
                "execution": item.get("spec", {}).get("execution"),
                "replicas": item.get("spec", {}).get("replicas"),
                "reason": item.get("status", {}).get("reason"),
                "integrationError": item.get("status", {}).get("integrationError"),
                "serviceObservation": item.get("status", {}).get("serviceObservation"),
                "conditions": [{**{key: condition.get(key) for key in ("type", "status", "reason")},
                                "category": condition_category(condition.get("message"))}
                               for condition in item.get("status", {}).get("conditions", [])],
                "privateScope": runtime_scope(setup, item) if label == "sandboxes" else None,
                "scheduling": scheduling_detail(item) if label == "pods" else None,
                "containers": [
                    {"name": container["name"], "ready": container.get("ready"),
                     "waitingReason": container.get("state", {}).get("waiting", {}).get("reason")}
                    for container in item.get("status", {}).get("containerStatuses", [])
                ],
            } for item in objects if not item["metadata"].get("namespace", "").startswith("kube-")]
        except Exception as error:
            result[label] = {"unavailable": type(error).__name__}
    try:
        raw = command("docker", "exec", "bridge-native-control-plane",
                      "cat", "/var/log/kars-native-audit/audit.log")
        errors = []
        for line in raw.splitlines():
            event = json.loads(line)
            status = event.get("responseStatus", {})
            if (event.get("stage") == "ResponseComplete"
                    and event.get("user", {}).get("username") == f"system:serviceaccount:{CORE}:kars-controller"
                    and status.get("code", 0) >= 400 and status["code"] != 404):
                ref = event.get("objectRef", {})
                errors.append({"verb": event["verb"], "code": status["code"],
                               "resource": ref.get("resource"), "namespace": ref.get("namespace"),
                               "name": ref.get("name"), "subresource": ref.get("subresource"),
                               "detail": status_detail(status)})
        result["recentControllerRejections"] = errors[-24:]
    except Exception as error:
        result["recentControllerRejections"] = {"unavailable": type(error).__name__}
    return result


def main():
    report = {"coreRevision": CORE_REVISION, "bridgeRevision": command("git", "rev-parse", "HEAD").strip(),
              "lane": "no-active-sre-native", "runtimeFixture": "controlled-no-LLM-agent",
              "workspaceContract": "ephemeral-filesystem-with-namespace-resource-continuity",
              "cases": {}, "runtimeQualified": False,
              "networkPolicyEnforcementQualified": False, "activeSreCombinedQualified": False}
    path = STATE / "evidence/native.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    setup = None

    def save():
        path.write_text(json.dumps(report, indent=2) + "\n")

    def case(name, operation, allowed=True):
        if not allowed:
            report["cases"][name] = {"result": "blocked", "reason": "native prerequisite did not complete"}
            save()
            return None
        start = time.monotonic()
        started_at = datetime.now(timezone.utc)
        try:
            value = operation()
            report["cases"][name] = {"result": "passed"}
            return value
        except Exception as error:
            report["cases"][name] = {
                "result": "failed",
                "failure": str(error) if isinstance(error, Failure) else type(error).__name__,
            }
            if setup:
                if name == "private-bff-observer-and-fresh-privacy-rpc":
                    report["cases"][name]["enrollmentTemplateDrift"] = template_diagnostics(
                        setup, observations.enrollment_templates, report["cases"][name]["failure"])
                    report["cases"][name]["observationReadiness"] = observation_diagnostics(
                        setup, observations.observer_target)
                    report["cases"][name]["actorApiOutcomes"] = api_outcome_diagnostics(
                        setup, "observer_router", observations.observer_target, started_at)
                elif name == "real-source-projection-and-runtime":
                    report["cases"][name]["actorApiOutcomes"] = api_outcome_diagnostics(
                        setup, "bff_writer", lifecycle.credential_diagnostic_target, started_at)
                report["cases"][name]["metadataAtFailure"] = diagnostics(setup)
                if name == "private-bff-observer-and-fresh-privacy-rpc":
                    # Persist the original failure before adding any diagnostic policy.
                    save()
                    report["cases"][name]["observerApiReachability"] = observer_network_diagnostics(
                        setup, observations.observer_target, report["cases"][name])
            return None
        finally:
            report["cases"][name]["seconds"] = round(time.monotonic() - start, 2)
            save()
            print(json.dumps({"nativeCase": name, **{key: value for key, value in report["cases"][name].items()
                                                   if key not in ("metadataAtFailure", "observationReadiness",
                                                                  "actorApiOutcomes", "observerApiReachability",
                                                                  "enrollmentTemplateDrift")}}), flush=True)

    def passed(name):
        return report["cases"].get(name, {}).get("result") == "passed"

    try:
        require(command("git", "-C", ".native/core", "rev-parse", "HEAD").strip() == CORE_REVISION
                and os.environ.get("CORE_REVISION") == CORE_REVISION,
                "Unreviewed core source cannot enter native acceptance")
        setup = Setup()
        install_core(setup)
        key = install_bridge(setup)
        with bridge_connection(key) as bff:
            bff.call("GET", f"/api/namespaces/{CORE}/channels", authenticated=False, expected=401)
            credentials = CredentialCases(setup, bff)
            case("live-controller-native-writer-readiness",
                 lambda: credentials.workspace("native-grant-preflight"))
            require(passed("live-controller-native-writer-readiness"),
                    "Live core did not issue native writer authority; see grant diagnostics")
            lifecycle = LifecycleCases(setup, bff, credentials)
            observations = ObservationCases(setup, bff, lifecycle)
            from grant_continuity_case import run as grant_continuity
            case("shared-workspace-and-active-grant-update-continuity",
                 lambda: grant_continuity(credentials))
            workspace = case("create-only-bootstrap-and-v1-preservation", credentials.bootstrap)
            case("unobserved-collision-no-adoption", credentials.collision)
            case("new-source-late-conflict-zero-mutations", lambda: credentials.late_conflict(False))
            case("existing-source-late-conflict-zero-mutations", lambda: credentials.late_conflict(True))
            case("persistent-removal-before-reviewed-legacy-import", credentials.removal_before_import)
            case("native-403-unregistered-namespace-role-alias",
                 lambda: credentials.native_denials(workspace), workspace is not None)
            case("real-source-projection-and-runtime", lifecycle.create_delivery)
            case("selected-source-revocation-and-uid-fence", lifecycle.source_uid_fence,
                 passed("real-source-projection-and-runtime"))
            case("team-rebind-uid-namespace-data-and-attestation-continuity", lifecycle.team_rebind)
            case("completed-team-fixture-release", lifecycle.release_team_fixture,
                 lifecycle.team_target is not None)
            case("private-bff-observer-and-fresh-privacy-rpc", observations.enable)
            case("purpose-only-pinned-tls-api-negative-matrix", observations.tls_api,
                 passed("private-bff-observer-and-fresh-privacy-rpc"))
            case("cni-9447-9448-positive-and-unauthorized-peer-denial", observations.cni_denials,
                 passed("private-bff-observer-and-fresh-privacy-rpc"))
            case("observer-rotation-current-bearer-and-revocation", observations.rotation,
                 passed("private-bff-observer-and-fresh-privacy-rpc"))
            case("writer-uninstall-held-name-and-delivery-continuity", lifecycle.writer_uninstall,
                 passed("real-source-projection-and-runtime"))
            case("grant-revocation-halts-consumer-retains-source", lifecycle.grant_revocation,
                 passed("real-source-projection-and-runtime"))
        report["runtimeQualified"] = all(item["result"] == "passed" for item in report["cases"].values())
        report["networkPolicyEnforcementQualified"] = passed(
            "cni-9447-9448-positive-and-unauthorized-peer-denial")
    except Exception as error:
        report["setupFailure"] = str(error) if isinstance(error, Failure) else type(error).__name__
    finally:
        if setup:
            report["diagnostics"] = diagnostics(setup)
        report["result"] = "passed" if report["runtimeQualified"] else "failed"
        save()
        print(json.dumps({key: report[key] for key in
                          ("coreRevision", "bridgeRevision", "workspaceContract", "result", "runtimeQualified",
                           "networkPolicyEnforcementQualified", "activeSreCombinedQualified")}), flush=True)
    return 0 if report["runtimeQualified"] else 1


if __name__ == "__main__":
    sys.exit(main())
