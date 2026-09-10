// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Native admission/creation proof only. No account state or Pod readiness is fabricated.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";

const kubeControllers = [
  ["manager", "system:kube-controller-manager"],
  ["deployment", "system:serviceaccount:kube-system:deployment-controller"],
  ["replicaset", "system:serviceaccount:kube-system:replicaset-controller"],
];
const api = "/api/v1";
const apps = "/apis/apps/v1";
const rbac = "/apis/rbac.authorization.k8s.io/v1";

export function policyDenial(response, policy, name) {
  const validation = policy.spec.validations[0];
  const reason = validation.reason ?? "Invalid";
  const expected = { Invalid: 422, Forbidden: 403 }[reason];
  const body = response.body;
  const message = `ValidatingAdmissionPolicy '${policy.name}' with binding '${policy.name}' denied request: ${validation.message}`;
  return response.status === expected && body?.kind === "Status" && body.status === "Failure"
    && body.reason === reason && body.details?.name === name
    && body.details?.causes?.some(cause => cause.message === message) === true;
}

export async function workloadCases(request, wait, { namespace, controller, principal, policy }, report) {
  assert(policy?.name === "kars-inference-budget-workloads" && policy.spec.validations.length === 1,
    "Expected the exact shared budget workload policy");
  const protectedNamespace = `${namespace}-runtime`;
  const owned = [];
  let primaryFailure = false;
  const emit = (name, response, expected) => report({
    case: name, httpStatus: response.status, expectedStatus: expected, matched: response.status === expected,
  });
  const create = async (path, body, actor) => {
    const response = await request("POST", path, body, actor);
    assert(response.status === 201 && response.body?.kind === body.kind
      && response.body.metadata?.name === body.metadata.name
      && response.body.metadata.namespace === body.metadata.namespace
      && typeof response.body.metadata.uid === "string"
      && typeof response.body.metadata.resourceVersion === "string", "Native workload fixture CREATE failed");
    owned.push([`${path}/${body.metadata.name}`, response.body.metadata.uid]);
    return response.body;
  };
  const podSpec = {
    automountServiceAccountToken: false, schedulerName: "budget-api-never-schedule",
    containers: [{ name: "probe", image: "registry.invalid/budget-api:never", imagePullPolicy: "Never" }],
  };
  const workload = (kind, name, label = name) => ({
    apiVersion: kind === "Pod" ? "v1" : "apps/v1", kind,
    metadata: { name, namespace: protectedNamespace, labels: { "budget-api-case": label } },
    spec: kind === "Pod" ? structuredClone(podSpec) : {
      replicas: kind === "Deployment" ? 1 : 0,
      selector: { matchLabels: { "budget-api-case": label } },
      template: { metadata: { labels: { "budget-api-case": label } }, spec: structuredClone(podSpec) },
    },
  });
  const collection = kind => kind === "Pod"
    ? `${api}/namespaces/${protectedNamespace}/pods`
    : `${apps}/namespaces/${protectedNamespace}/${kind === "Deployment" ? "deployments" : "replicasets"}`;
  const expectDenied = async (caseName, method, path, body, actor) => {
    const response = await request(method, path + "?dryRun=All", body, actor);
    assert(policyDenial(response, policy, body.metadata.name), `Wrong native denial for ${caseName}`);
    emit(caseName, response, policy.spec.validations[0].reason === "Forbidden" ? 403 : 422);
  };
  try {
    for (const actor of [controller, principal, ...kubeControllers.map(([, actor]) => actor)]) {
      if (actor.startsWith("system:serviceaccount:")) {
        const [, , ns, name] = actor.split(":");
        const actual = await request("GET", `${api}/namespaces/${ns}/serviceaccounts/${name}`);
        assert(actual.status === 200 && actual.body?.metadata?.uid, "Native controller ServiceAccount missing");
      }
    }
    const ns = await create(`${api}/namespaces`, {
      apiVersion: "v1", kind: "Namespace", metadata: { name: protectedNamespace },
    });
    const labelRole = `${protectedNamespace}-label`;
    await create(`${rbac}/clusterroles`, {
      apiVersion: "rbac.authorization.k8s.io/v1", kind: "ClusterRole", metadata: { name: labelRole },
      rules: [{ apiGroups: [""], resources: ["namespaces"], resourceNames: [protectedNamespace], verbs: ["get", "patch"] }],
    });
    await create(`${rbac}/clusterrolebindings`, {
      apiVersion: "rbac.authorization.k8s.io/v1", kind: "ClusterRoleBinding", metadata: { name: labelRole },
      roleRef: { apiGroup: "rbac.authorization.k8s.io", kind: "ClusterRole", name: labelRole },
      subjects: [{ kind: "User", apiGroup: "rbac.authorization.k8s.io", name: controller }],
    });
    await create(`${rbac}/namespaces/${protectedNamespace}/roles`, {
      apiVersion: "rbac.authorization.k8s.io/v1", kind: "Role",
      metadata: { name: "admission-proof", namespace: protectedNamespace },
      rules: [
        { apiGroups: [""], resources: ["pods", "pods/ephemeralcontainers"], verbs: ["get", "create", "update", "patch"] },
        { apiGroups: ["apps"], resources: ["deployments", "replicasets"], verbs: ["get", "create"] },
      ],
    });
    const actors = [controller, principal, ...kubeControllers.map(([, actor]) => actor)];
    await create(`${rbac}/namespaces/${protectedNamespace}/rolebindings`, {
      apiVersion: "rbac.authorization.k8s.io/v1", kind: "RoleBinding",
      metadata: { name: "admission-proof", namespace: protectedNamespace },
      roleRef: { apiGroup: "rbac.authorization.k8s.io", kind: "Role", name: "admission-proof" },
      subjects: actors.map(name => ({ kind: "User", apiGroup: "rbac.authorization.k8s.io", name })),
    });
    const permission = async (actor, group, resource, verb, subresource, ns = protectedNamespace, name) => {
      const response = await request("POST", "/apis/authorization.k8s.io/v1/subjectaccessreviews", {
        apiVersion: "authorization.k8s.io/v1", kind: "SubjectAccessReview",
        spec: { user: actor, resourceAttributes: { namespace: ns, group, resource, verb,
          ...(subresource ? { subresource } : {}), ...(name ? { name } : {}) } },
      });
      return response.status === 201 && response.body?.status?.allowed === true
        && !response.body.status.evaluationError;
    };
    await wait(() => permission(controller, "", "namespaces", "patch", undefined, "", protectedNamespace),
      "core fixture namespace-label authority");
    const labeled = await request("PATCH", `${api}/namespaces/${protectedNamespace}`, {
      metadata: { uid: ns.metadata.uid, resourceVersion: ns.metadata.resourceVersion,
        labels: { "kars.azure.com/inference-budget": "v1" } },
    }, controller);
    assert(labeled.status === 200 && labeled.body?.metadata?.uid === ns.metadata.uid
      && labeled.body.metadata.labels?.["kars.azure.com/inference-budget"] === "v1",
    "Native budget namespace fence was not installed");
    for (const actor of actors) {
      for (const [group, resource, verb, subresource] of [
        ["", "pods", "create"], ["apps", "deployments", "create"], ["apps", "replicasets", "create"],
        ["", "pods", "update", "ephemeralcontainers"],
      ]) {
        await wait(() => permission(actor, group, resource, verb, subresource), "workload fixture RBAC");
      }
    }
    await wait(async () => {
      const response = await request("GET", `${api}/namespaces/${protectedNamespace}/serviceaccounts/default`);
      return response.status === 200 && response.body?.metadata?.uid;
    }, "native default ServiceAccount");
    // This is the real built-in controller chain, not an impersonated/fabricated child.
    const deployment = await create(collection("Deployment"), workload("Deployment", "actual-chain"), controller);
    emit("core-deployment-primary", { status: 201 }, 201);
    let replicaSet, pod;
    await wait(async () => {
      const response = await request("GET", collection("ReplicaSet") + "?labelSelector=budget-api-case%3Dactual-chain");
      replicaSet = response.body?.items?.find(item => item.metadata?.ownerReferences?.some(
        owner => owner.controller === true && owner.uid === deployment.metadata.uid));
      return response.status === 200 && replicaSet?.metadata?.uid;
    }, "native Deployment controller creates budget ReplicaSet");
    await wait(async () => {
      const response = await request("GET", collection("Pod") + "?labelSelector=budget-api-case%3Dactual-chain");
      pod = response.body?.items?.find(item => item.metadata?.ownerReferences?.some(
        owner => owner.controller === true && owner.uid === replicaSet.metadata.uid));
      return response.status === 200 && pod?.metadata?.uid;
    }, "native ReplicaSet controller creates budget Pod");
    assert(pod.spec.automountServiceAccountToken === false && !pod.metadata.deletionTimestamp,
      "Native chain must retain its non-executing tokenless fixture");
    report({ case: "actual-controller-chain-created", matched: true, readinessClaimed: false });

    const target = await create(collection("Pod"), workload("Pod", "ephemeral-target"), controller);
    emit("core-pod-primary", { status: 201 }, 201);
    for (const [label, actor] of kubeControllers) {
      for (const kind of ["ReplicaSet", "Pod"]) {
        const created = await create(collection(kind), workload(kind, `${label}-${kind.toLowerCase()}`), actor);
        emit(`${label}-${kind.toLowerCase()}-primary`, { status: 201 }, 201);
        assert(created.metadata.uid, "Native primary creation omitted UID");
      }
      await expectDenied(`${label}-deployment-denied`, "POST", collection("Deployment"),
        workload("Deployment", `denied-${label}-deployment`), actor);
    }
    for (const kind of ["Deployment", "ReplicaSet", "Pod"]) {
      await expectDenied(`tenant-${kind.toLowerCase()}-denied`, "POST", collection(kind),
        workload(kind, `denied-tenant-${kind.toLowerCase()}`), principal);
    }
    for (const [label, actor] of [...kubeControllers, ["tenant", principal]]) {
      const response = await request("GET", collection("Pod") + "/" + target.metadata.name);
      assert(response.status === 200 && response.body?.metadata?.uid === target.metadata.uid,
        "Ephemeral-container target changed");
      const update = structuredClone(response.body);
      update.spec.ephemeralContainers = [{
        name: "denied-ephemeral", image: "registry.invalid/budget-api:never",
        imagePullPolicy: "Never", targetContainerName: "probe",
      }];
      await expectDenied(`${label}-ephemeral-denied`, "PUT",
        collection("Pod") + "/" + target.metadata.name + "/ephemeralcontainers", update, actor);
    }
  } catch (error) {
    primaryFailure = true;
    throw error;
  } finally {
    let cleanupFailed = false;
    for (const [path, uid] of owned.reverse()) {
      try {
        let removed = false;
        for (let attempt = 0; attempt < 3; attempt++) {
          const current = await request("GET", path);
          if (current.status === 404) { removed = true; break; }
          assert(current.status === 200 && current.body?.metadata?.uid === uid, "Workload fixture cleanup UID changed");
          const result = await request("DELETE", path, {
            apiVersion: "v1", kind: "DeleteOptions",
            preconditions: { uid, resourceVersion: current.body.metadata.resourceVersion },
            propagationPolicy: "Background",
          });
          if (result.status === 409) continue;
          assert([200, 202].includes(result.status), "Workload fixture cleanup failed");
          removed = true;
          break;
        }
        assert(removed, "Workload fixture cleanup contention exceeded its bound");
      } catch {
        cleanupFailed = true;
      }
    }
    if (cleanupFailed && !primaryFailure) throw new Error("Native workload fixture cleanup incomplete");
  }
}

export async function runWorkloadProof(options) {
  const { root, context, kubectl, until } = options;
  const config = JSON.parse(kubectl(["config", "view", "--minify", "-o", "json"]));
  assert(config.contexts?.length === 1 && config.contexts[0].name === context
    && config.clusters?.length === 1
    && ["localhost", "127.0.0.1", "[::1]"].includes(new URL(config.clusters[0].cluster.server).hostname),
  "Budget workload proof requires the exact loopback Kind context");
  const proxy = spawn("kubectl", ["--context", context, "--request-timeout=20s", "proxy",
    "--address=127.0.0.1", "--port=0"], { cwd: root, stdio: ["ignore", "pipe", "pipe"] });
  let output = "", port;
  proxy.stdout.on("data", data => { output = (output + data).slice(-2048); });
  proxy.stderr.on("data", () => {});
  try {
    await until(() => {
      assert(proxy.exitCode === null, "Budget API proxy exited");
      port = output.match(/127\.0\.0\.1:(\d+)/)?.[1];
      return Boolean(port);
    }, "budget workload API proxy");
    const request = async (method, path, body, actor) => {
      const response = await fetch(`http://127.0.0.1:${port}${path}`, {
        method, signal: AbortSignal.timeout(15_000),
        headers: { "Content-Type": method === "PATCH" ? "application/merge-patch+json" : "application/json",
          Accept: "application/json", ...(actor ? { "Impersonate-User": actor, "Impersonate-Group": "system:authenticated" } : {}) },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      return { status: response.status, body: await response.json().catch(() => null) };
    };
    await workloadCases(request, until, options,
      result => console.log("BUDGET-WORKLOAD " + JSON.stringify(result)));
  } finally {
    if (proxy.exitCode === null) {
      proxy.kill("SIGTERM");
      await new Promise(resolve => {
        const timer = setTimeout(() => { proxy.kill("SIGKILL"); resolve(); }, 5000);
        proxy.once("exit", () => { clearTimeout(timer); resolve(); });
      });
    }
  }
}
