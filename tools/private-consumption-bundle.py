#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Render the shared, passive-until-activation private consumption contract."""

import argparse
import json
from pathlib import Path

PREFIX = "kars.azure.com/private-"
CONTROLLERS = (
    "deployment-controller", "replicaset-controller", "replication-controller",
    "statefulset-controller", "daemon-set-controller", "job-controller", "cronjob-controller",
)
SECRETS = (
    "router-services-admin", "router-services-observer", "router-services-observer-identity",
    "router-github-app", "kars-observation-privacy-tls", "sre-api-router-identity",
)
TOKEN_AUDIENCES = ("kars.azure.com/governed-inference-budget",)


def variable(name, expression):
    return {"name": name, "expression": expression}


def pair(name, rules, variables, validations, conditions=None):
    spec = {
        "failurePolicy": "Fail",
        "matchConstraints": {
            "matchPolicy": "Equivalent", "namespaceSelector": {}, "objectSelector": {},
            "resourceRules": rules,
        },
        "variables": variables,
        "validations": [{"expression": expression, "message": message, "reason": "Forbidden"}
                        for expression, message in validations],
    }
    if conditions:
        spec["matchConditions"] = conditions
    return [
        {"apiVersion": "admissionregistration.k8s.io/v1", "kind": "ValidatingAdmissionPolicy",
         "metadata": {"name": name, "annotations": {"helm.sh/resource-policy": "keep"}}, "spec": spec},
        {"apiVersion": "admissionregistration.k8s.io/v1", "kind": "ValidatingAdmissionPolicyBinding",
         "metadata": {"name": name, "annotations": {"helm.sh/resource-policy": "keep"}},
         "spec": {"policyName": name, "validationActions": ["Deny", "Audit"]}},
    ]


def rule(group, version, resources, scope="Namespaced"):
    return {"apiGroups": [group], "apiVersions": [version], "operations": ["CREATE", "UPDATE"],
            "resources": resources, "scope": scope}

def activation_schema():
    def object_schema(properties, required):
        return {"type": "object", "properties": properties, "required": required}
    text = {"type": "string", "minLength": 1, "maxLength": 253}
    digest = {"type": "string", "pattern": "^[a-f0-9]{64}$"}
    identity = object_schema({"name": text, "uid": text, "resourceVersion": text},
                             ["name", "uid", "resourceVersion"])
    consumer = object_schema({
        "kind": {"type": "string", "enum": ["Deployment", "ReplicaSet", "StatefulSet", "DaemonSet",
                                                   "ReplicationController", "Job", "CronJob", "Pod"]},
        "object": identity, "templateDigest": digest,
    }, ["kind", "object", "templateDigest"])
    namespace = object_schema({
        "namespace": identity, "consumers": {"type": "array", "maxItems": 64, "items": consumer}, "epoch": digest,
    }, ["namespace", "consumers"])
    root = object_schema({"namespace": identity, "account": identity, "deployment": identity,
                          "templateDigest": digest,
                          "budgetTls": object_schema({"namespace": identity, "secret": identity, "keyDigest": digest},
                                                     ["namespace", "secret", "keyDigest"])},
                         ["namespace", "account", "deployment", "templateDigest"])
    return object_schema({
        "contract": {"type": "string", "enum": ["kars.azure.com/private-consumption/v1"]},
        "phase": {"type": "string", "enum": ["reviewed", "qualified"]},
        "bundleRevision": digest, "root": root,
        "profile": {"type": "string", "enum": ["service-accounts", "kcm-certificate"]},
        "controllerUids": {"type": "object", "additionalProperties": text},
        "namespaces": {"type": "array", "minItems": 1, "maxItems": 64, "items": namespace},
    }, ["contract", "phase", "bundleRevision", "root", "profile", "controllerUids", "namespaces"])


def bundle():
    a = "variables.a"
    epoch = f"{a}[?'{PREFIX}epoch'].orValue('')"
    manager = ("authorizer.group('kars.azure.com').resource('karscredentialgrants')"
               ".name('workspace').check('manage').allowed()")
    projector = (
        f"request.userInfo.username == {a}[?'{PREFIX}root-user'].orValue('') && "
        f"has(request.userInfo.uid) && request.userInfo.uid != '' && "
        f"request.userInfo.uid == {a}[?'{PREFIX}root-uid'].orValue('') && "
        "authorizer.group('kars.azure.com').resource('karscredentialgrants')"
        ".name('workspace').check('project-credentials').allowed()"
    )
    metadata = "namespaceObject.metadata.?annotations.orValue({})"
    material = (
        "variables.pods.exists(p, "
        "p.?volumes.orValue([]).exists(v, "
        "(has(v.secret) && v.secret.secretName in variables.secrets) || "
        "(has(v.projected) && v.projected.sources.exists(s, "
        "(has(s.secret) && s.secret.name in variables.secrets) || "
        "(has(s.serviceAccountToken) && s.serviceAccountToken.?audience.orValue('') in variables.tokenAudiences))) || "
        "(has(v.csi) && has(v.csi.nodePublishSecretRef) && v.csi.nodePublishSecretRef.name in variables.secrets) || "
        "['azureFile','cephfs','cinder','flexVolume','iscsi','rbd','scaleIO','storageos'].exists(k, "
        "k in v && (('secretName' in v[k] && v[k].secretName in variables.secrets) || "
        "('secretRef' in v[k] && v[k].secretRef.name in variables.secrets)))) || "
        "p.?imagePullSecrets.orValue([]).exists(s, s.name in variables.secrets) || "
        "(p.?containers.orValue([]) + p.?initContainers.orValue([]) + p.?ephemeralContainers.orValue([])).exists(c, "
        "c.?envFrom.orValue([]).exists(e, has(e.secretRef) && e.secretRef.name in variables.secrets) || "
        "c.?env.orValue([]).exists(e, has(e.valueFrom) && has(e.valueFrom.secretKeyRef) && "
        "e.valueFrom.secretKeyRef.name in variables.secrets)))"
    )
    privileged = (
        "variables.pods.exists(p, p.?hostNetwork.orValue(false) || p.?hostPID.orValue(false) || "
        "p.?hostIPC.orValue(false) || p.?volumes.orValue([]).exists(v, has(v.hostPath)) || "
        "(p.?containers.orValue([]) + p.?initContainers.orValue([]) + p.?ephemeralContainers.orValue([])).exists(c, "
        "c.?securityContext.privileged.orValue(false) || "
        "c.?securityContext.capabilities.add.orValue([]).exists(k, k in "
        "['ALL','SYS_ADMIN','SYS_PTRACE','SYS_MODULE','SYS_RAWIO','BPF','PERFMON','CHECKPOINT_RESTORE','DAC_READ_SEARCH'])))"
    )
    identity = (
        "variables.pods.exists(p, "
        f"((request.namespace == {a}[?'{PREFIX}root-namespace'].orValue('') && "
        f"p.?serviceAccountName.orValue('') == {a}[?'{PREFIX}root-account'].orValue('')) || "
        "(request.namespace == 'kars-sre' && p.?serviceAccountName.orValue('') == 'sre-api-router') || "
        "(request.namespace == 'kube-system' && p.?serviceAccountName.orValue('') in variables.controllers)) && "
        "(p.?automountServiceAccountToken.orValue(true) || p.?volumes.orValue([]).exists(v, "
        "has(v.projected) && v.projected.sources.exists(s, has(s.serviceAccountToken)))))"
    )
    stage = (
        "variables.owners.size() != 1 ? '' : "
        "(request.resource.group == 'apps' && request.resource.resource == 'replicasets' && "
        "variables.owners[0].apiVersion == 'apps/v1' && variables.owners[0].kind == 'Deployment') ? 'deployment-controller' : "
        "(request.resource.group == 'batch' && request.resource.resource == 'jobs' && "
        "variables.owners[0].apiVersion == 'batch/v1' && variables.owners[0].kind == 'CronJob') ? 'cronjob-controller' : "
        "(request.resource.group == '' && request.resource.resource == 'pods') ? "
        "(variables.owners[0].apiVersion == 'apps/v1' && variables.owners[0].kind == 'ReplicaSet' ? 'replicaset-controller' : "
        "variables.owners[0].apiVersion == 'v1' && variables.owners[0].kind == 'ReplicationController' ? 'replication-controller' : "
        "variables.owners[0].apiVersion == 'apps/v1' && variables.owners[0].kind == 'StatefulSet' ? 'statefulset-controller' : "
        "variables.owners[0].apiVersion == 'apps/v1' && variables.owners[0].kind == 'DaemonSet' ? 'daemon-set-controller' : "
        "variables.owners[0].apiVersion == 'batch/v1' && variables.owners[0].kind == 'Job' ? 'job-controller' : '') : ''"
    )
    variables = [
        variable("a", metadata),
        variable("objects", "[object, oldObject].filter(o, o != null).map(o, dyn(o))"),
        variable("templates", "variables.objects.map(o, o.kind == 'Pod' ? o : "
                 "o.kind == 'CronJob' ? o.spec.jobTemplate.spec.template : "
                 "has(o.spec.template) ? o.spec.template : null).filter(t, t != null)"),
        variable("pods", "variables.templates.map(t, t.spec)"),
        variable("secrets", repr(list(SECRETS)) +
                 f" + (request.namespace == {a}[?'{PREFIX}budget-namespace'].orValue('') ? "
                 f"[{a}[?'{PREFIX}budget-tls-name'].orValue('')].filter(n, n != '') : [])"),
        variable("tokenAudiences", repr(list(TOKEN_AUDIENCES))),
        variable("controllers", repr(list(CONTROLLERS))),
        variable("material", material),
        variable("privileged", privileged),
        variable("identity", identity),
        variable("marked", f"variables.templates.exists(t, '{PREFIX}epoch' in t.metadata.?annotations.orValue({{}}))"),
        variable("owners", "object.metadata.?ownerReferences.orValue([]).filter(o, o.?controller.orValue(false))"),
        variable("stage", stage),
        variable("manager", f"({manager}) || (request.namespace == 'kars-sre' && "
                 "authorizer.group('kars.azure.com').resource('karssreregistrations').name('canonical').check('use').allowed())"),
        variable("projector", projector),
        variable("authenticatedStage", "variables.stage != '' && "
                 f"(({a}[?'{PREFIX}profile'].orValue('') == 'kcm-certificate' && "
                 "request.userInfo.username == 'system:kube-controller-manager' && "
                 "(!has(request.userInfo.uid) || request.userInfo.uid == '')) || "
                 f"({a}[?'{PREFIX}profile'].orValue('') == 'service-accounts' && "
                 "request.userInfo.username == 'system:serviceaccount:kube-system:' + variables.stage && "
                 "has(request.userInfo.uid) && request.userInfo.uid != '' && "
                 f"request.userInfo.uid == {a}[?('{PREFIX}' + variables.stage + '-uid')].orValue(''))) && "
                 "authorizer.group(request.resource.group).resource(request.resource.resource)"
                 ".check(request.operation == 'CREATE' ? 'create' : 'update').allowed()"),
        variable("sameTemplate", "oldObject != null && "
                 "object.metadata.?ownerReferences.orValue([]) == oldObject.metadata.?ownerReferences.orValue([]) && "
                 "(object.kind == 'Pod' ? object.spec == oldObject.spec && "
                 f"object.metadata.?annotations.orValue({{}})[?'{PREFIX}epoch'].orValue('') == "
                 f"oldObject.metadata.?annotations.orValue({{}})[?'{PREFIX}epoch'].orValue('') : "
                 "object.kind == 'CronJob' ? object.spec.jobTemplate == oldObject.spec.jobTemplate : "
                 "dyn(object).spec.?template.orValue(null) == dyn(oldObject).spec.?template.orValue(null))"),
        variable("fresh", f"{a}[?'{PREFIX}state'].orValue('') == 'Qualified' && {epoch} != '' && "
                 f"{a}[?'{PREFIX}namespace-uid'].orValue('') == dyn(namespaceObject.metadata).uid && "
                 f"(variables.templates.all(t, t.metadata.?annotations.orValue({{}})[?'{PREFIX}epoch'].orValue('') == {epoch}) || "
                 f"(variables.owners.size() == 1 && {a}[?('{PREFIX}parent-' + variables.owners[0].uid)].orValue('') == {epoch}))"),
        variable("retiring", "request.operation == 'UPDATE' && variables.sameTemplate && "
                 "((request.resource.resource == 'replicasets' && object.spec.?replicas.orValue(1) == 0) || "
                 "(request.resource.resource == 'pods' && has(oldObject.metadata.deletionTimestamp)))"),
    ]
    output = pair(
        "kars-private-consumption",
        [rule("", "v1", ["pods", "pods/ephemeralcontainers", "replicationcontrollers"]),
         rule("apps", "v1", ["deployments", "replicasets", "statefulsets", "daemonsets"]),
         rule("batch", "v1", ["jobs", "cronjobs"])],
        variables,
        [("!(variables.material || variables.identity || variables.privileged || variables.marked) || "
          "variables.manager || variables.projector || "
          "(variables.authenticatedStage && request.?subResource.orValue('') != 'ephemeralcontainers' && "
          "(variables.retiring || (variables.fresh && (request.operation == 'CREATE' || variables.sameTemplate))))",
          "Private capability consumption requires qualified actor authority; an epoch alone grants none")],
        [{"name": "activated-private-namespace",
          "expression": f"{metadata}[?'{PREFIX}enabled'].orValue('') == 'true'"}],
    )
    output += pair(
        "kars-private-consumption-namespace",
        [rule("", "v1", ["namespaces", "namespaces/status", "namespaces/finalize"], "Cluster")],
        [variable("a", "object.metadata.?annotations.orValue({})"), variable("manager", manager),
         variable("projector", projector)],
        [("request.operation == 'UPDATE' && (variables.manager || variables.projector)",
          "Only private capability operators and the exact authorized projector may stage namespace fences"),
         (f"oldObject == null || oldObject.metadata.?annotations.orValue({{}})[?'{PREFIX}enabled'].orValue('') != 'true' || "
          f"{a}[?'{PREFIX}enabled'].orValue('') == 'true'",
          "Private namespace protection is retained during authority retirement"),
         (f"request.operation == 'UPDATE' && {a}[?'{PREFIX}namespace-uid'].orValue('') == dyn(object.metadata).uid",
          "Private activation is bound to the actual namespace UID")],
        [{"name": "private-fence-fields",
          "expression": f"oldObject == null ? object.metadata.?annotations.orValue({{}}).exists(k, k.startsWith('{PREFIX}')) : "
          f"[object, oldObject].exists(o, o.metadata.?annotations.orValue({{}}).exists(k, k.startsWith('{PREFIX}') && "
          "object.metadata.?annotations.orValue({})[?k].orValue('') != oldObject.metadata.?annotations.orValue({})[?k].orValue('')))"}],
    )
    connect = rule("", "v1", ["pods/exec", "pods/attach", "pods/portforward", "pods/proxy"])
    connect["operations"] = ["CONNECT"]
    output += pair(
        "kars-private-consumption-connect", [connect],
        [variable("a", metadata), variable("manager", manager), variable("projector", projector)],
        [("variables.manager || variables.projector || (request.namespace == 'kars-sre' && "
          "authorizer.group('kars.azure.com').resource('karssreregistrations').name('canonical').check('use').allowed())",
          "Private capability namespaces require explicit operator authority for workload connections")],
        [{"name": "activated-private-namespace",
          "expression": f"{metadata}[?'{PREFIX}enabled'].orValue('') == 'true'"}],
    )
    output += pair(
        "kars-private-consumption-grant",
        [rule("kars.azure.com", "v1alpha1", ["karscredentialgrants", "karscredentialgrants/status"])],
        [variable("a", metadata)],
        [("request.?subResource.orValue('') == 'status' || "
          "(request.operation == 'UPDATE' && object.spec == oldObject.spec) || "
          "!object.spec.?enabled.orValue(true) || object.spec.writers.size() == 0 || "
          "(has(object.spec.privateActivation) && object.spec.privateActivation.contract == "
          "'kars.azure.com/private-consumption/v1' && object.spec.privateActivation.phase == 'qualified' && "
          f"{a}[?'{PREFIX}enabled'].orValue('') == 'true' && {a}[?'{PREFIX}state'].orValue('') == 'Qualified' && "
          f"object.spec.privateActivation.bundleRevision == {a}[?'{PREFIX}bundle-revision'].orValue('') && "
          "object.spec.privateActivation.namespaces.exists(n, n.namespace.name == request.namespace && "
          f"n.namespace.uid == dyn(namespaceObject.metadata).uid && n.?epoch.orValue('') == {epoch}))",
          "Private writers require upgraded reviewed activation; re-preview and qualify before enrollment"),
         ("request.?subResource.orValue('') != 'status' || "
          "!object.status.?conditions.orValue([]).exists(c, c.type == 'WriterReady' && c.status == 'True') || "
          "(has(object.spec.privateActivation) && object.spec.privateActivation.phase == 'qualified' && "
          f"{a}[?'{PREFIX}state'].orValue('') == 'Qualified' && "
          f"object.spec.privateActivation.bundleRevision == {a}[?'{PREFIX}bundle-revision'].orValue('') && "
          "object.status.?conditions.orValue([]).exists(c, c.type == 'PrivateConsumptionReady' && c.status == 'True' && "
          "c.observedGeneration == object.metadata.generation))",
          "Private writer Ready requires the current consumption qualification condition")],
    )
    return {"contract": "kars.azure.com/private-consumption/v1", "secrets": list(SECRETS), "tokenAudiences": list(TOKEN_AUDIENCES),
            "controllers": list(CONTROLLERS), "activationSchema": activation_schema(), "objects": output}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    destination = Path(__file__).resolve().parents[1] / "deploy/helm/kars/files/private-consumption.json"
    content = json.dumps(bundle(), indent=2) + "\n"
    if args.check:
        if destination.read_text() != content:
            raise SystemExit("Private consumption bundle differs from its canonical source")
    else:
        destination.write_text(content)


if __name__ == "__main__":
    main()
