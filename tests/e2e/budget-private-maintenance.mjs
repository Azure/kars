// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Native CEL proof in a unique namespace. Fixtures cannot execute or receive tokens.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { randomBytes } from "node:crypto";

const [kubeconfig, context] = process.argv.slice(2);
assert(kubeconfig && /^kind-kars-/.test(context ?? ""), "Explicit Kars Kind kubeconfig and context required");
const base = ["--kubeconfig", kubeconfig, "--context", context, "--request-timeout=20s"];
const run = (args, body, actor) => execFileSync("kubectl", [...base,
  ...(actor ? [`--as=${actor}`, "--as-group=system:authenticated"] : []), ...args], {
  input: body === undefined ? undefined : JSON.stringify(body), encoding: "utf8", timeout: 30_000,
  stdio: ["pipe", "pipe", "pipe"],
});
const get = (kind, name, namespace) => JSON.parse(run(["get", kind, name,
  ...(namespace ? ["-n", namespace] : []), "-o", "json"]));
const config = JSON.parse(run(["config", "view", "--minify", "-o", "json"]));
assert(config.contexts?.[0]?.name === context && ["127.0.0.1", "localhost", "[::1]"].includes(
  new URL(config.clusters[0].cluster.server).hostname), "Local Kind API required");
const name = `kars-budget-maint-${randomBytes(6).toString("hex")}`;
const operator = `${name}:operator`, tenant = `${name}:tenant`;
const owned = [];
const create = body => {
  const object = JSON.parse(run(["create", "-f", "-", "-o", "json"], body));
  owned.push({ kind: object.kind, name: object.metadata.name, namespace: object.metadata.namespace, uid: object.metadata.uid });
  return object;
};
const replace = (object, dryRun = false, actor) => JSON.parse(run(["replace", "-f", "-", "-o", "json",
  ...(dryRun ? ["--dry-run=server"] : [])], object, actor));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const policy = JSON.parse(readFileSync(new URL("../../deploy/helm/kars/files/inference-budget-admission.json", import.meta.url), "utf8")
  .replaceAll("__ACCOUNTING_NAMESPACE__", name)).items.find(p => p.name === "kars-inference-budget-workloads");
policy.spec.matchConstraints.namespaceSelector = { matchLabels: { "kubernetes.io/metadata.name": name } };
const denial = `ValidatingAdmissionPolicy '${name}' with binding '${name}' denied request: ${policy.spec.validations[0].message}`;
const results = [];
function check(label, mutate, allowed, actor, privateDenial = false) {
  const object = get("deployment", "probe", name);
  mutate(object);
  let denyingPolicy;
  try {
    replace(object, true, actor);
    assert(allowed, `${label}: unexpectedly allowed`);
  } catch (error) {
    const message = String(error.stderr ?? "");
    if (allowed) throw error;
    if (message.includes(denial)) denyingPolicy = name;
    else if (privateDenial && message.includes("ValidatingAdmissionPolicy 'kars-private-consumption' with binding 'kars-private-consumption' denied request: Private capability consumption requires qualified actor authority; an epoch alone grants none")) denyingPolicy = "kars-private-consumption";
    else throw error;
  }
  results.push({ case: label, allowed, matched: true, ...(denyingPolicy ? { denyingPolicy } : {}) });
  console.log(JSON.stringify(results.at(-1)));
}
function annotate(values) {
  const ns = get("namespace", name);
  ns.metadata.annotations = { ...ns.metadata.annotations, ...values };
  return replace(ns);
}
const prefix = "kars.azure.com/private-";
let primaryError;
try {
  const ns = create({ apiVersion: "v1", kind: "Namespace", metadata: { name } });
  create({ apiVersion: "rbac.authorization.k8s.io/v1", kind: "Role", metadata: { name: "fixture", namespace: name },
    rules: [{ apiGroups: ["apps"], resources: ["deployments"], verbs: ["get", "update"] }] });
  create({ apiVersion: "rbac.authorization.k8s.io/v1", kind: "RoleBinding", metadata: { name: "fixture", namespace: name },
    roleRef: { apiGroup: "rbac.authorization.k8s.io", kind: "Role", name: "fixture" },
    subjects: [operator, tenant].map(name => ({ kind: "User", apiGroup: "rbac.authorization.k8s.io", name })) });
  create({ apiVersion: "rbac.authorization.k8s.io/v1", kind: "Role", metadata: { name: "manager", namespace: name },
    rules: [{ apiGroups: ["kars.azure.com"], resources: ["karscredentialgrants"], resourceNames: ["workspace"], verbs: ["manage"] }] });
  create({ apiVersion: "rbac.authorization.k8s.io/v1", kind: "RoleBinding", metadata: { name: "manager", namespace: name },
    roleRef: { apiGroup: "rbac.authorization.k8s.io", kind: "Role", name: "manager" },
    subjects: [{ kind: "User", apiGroup: "rbac.authorization.k8s.io", name: operator }] });
  const deployment = create({ apiVersion: "apps/v1", kind: "Deployment", metadata: { name: "probe", namespace: name },
    spec: { replicas: 1, selector: { matchLabels: { probe: name } },
      template: { metadata: { labels: { probe: name }, annotations: { preserve: "yes" } }, spec: {
        automountServiceAccountToken: false, schedulerName: "budget-api-never-schedule",
        containers: [{ name: "probe", image: "registry.invalid/budget-api:never", imagePullPolicy: "Never" }],
      } } } });
  annotate({ [prefix + "enabled"]: "true", [prefix + "namespace-uid"]: ns.metadata.uid,
    [prefix + "root-namespace"]: name, [prefix + "state"]: "Pending" });
  create({ apiVersion: "admissionregistration.k8s.io/v1", kind: "ValidatingAdmissionPolicy", metadata: { name }, spec: policy.spec });
  let compiled = false;
  for (let i = 0; i < 40; i++) {
    const live = get("validatingadmissionpolicy", name);
    if (live.status?.observedGeneration === live.metadata.generation) {
      assert(!live.status.typeChecking?.expressionWarnings?.length, JSON.stringify(live.status));
      compiled = true; break;
    }
    await sleep(250);
  }
  assert(compiled, "Native policy type checking did not finish");
  create({ apiVersion: "admissionregistration.k8s.io/v1", kind: "ValidatingAdmissionPolicyBinding", metadata: { name },
    spec: { policyName: name, validationActions: ["Deny"] } });
  let enforced = false;
  for (let i = 0; i < 40; i++) {
    try { const d = get("deployment", "probe", name); d.spec.replicas = 0; replace(d, true, tenant); }
    catch (error) { if (String(error.stderr ?? "").includes(denial)) { enforced = true; break; } throw error; }
    await sleep(250);
  }
  assert(enforced, "Native policy binding did not become effective");
  const pause = d => { d.spec.replicas = 0; };
  check("pending-operator-pause", pause, true);
  check("pending-tenant-pause", pause, false, tenant);
  check("pending-nonzero", d => { d.spec.replicas = 2; }, false);
  check("pending-image-change", d => { pause(d); d.spec.template.spec.containers[0].image = "registry.invalid/changed:never"; }, false);
  check("pending-strategy-change", d => { pause(d); d.spec.minReadySeconds = 12; }, false);
  check("pending-metadata-change", d => { pause(d); d.metadata.labels = { changed: "yes" }; }, false);
  annotate({ [prefix + "root-namespace"]: "wrong-root" });
  check("wrong-root-pause", pause, false);
  annotate({ [prefix + "root-namespace"]: name });
  const stopped = get("deployment", "probe", name); pause(stopped); replace(stopped, false, operator);
  const epoch = "a".repeat(64);
  annotate({ [prefix + "state"]: "Qualified", [prefix + "epoch"]: epoch, [prefix + "parent-" + deployment.metadata.uid]: epoch });
  const qualify = d => { d.spec.template.metadata.annotations[prefix + "epoch"] = epoch; };
  check("qualified-operator-epoch", qualify, true);
  check("qualified-tenant-epoch", qualify, false, tenant, true);
  check("qualified-tenant-unmarked", () => {}, false, tenant);
  check("qualified-wrong-epoch", d => { d.spec.template.metadata.annotations[prefix + "epoch"] = "b".repeat(64); }, false);
  check("qualified-nonzero", d => { qualify(d); d.spec.replicas = 1; }, false);
  check("qualified-image-change", d => { qualify(d); d.spec.template.spec.containers[0].image = "registry.invalid/changed:never"; }, false);
  check("qualified-other-annotation", d => { qualify(d); delete d.spec.template.metadata.annotations.preserve; }, false);
  check("qualified-template-label", d => { qualify(d); d.spec.template.metadata.labels.changed = "yes"; }, false);
  check("qualified-metadata-change", d => { qualify(d); d.metadata.annotations = { changed: "yes" }; }, false);
  annotate({ [prefix + "parent-" + deployment.metadata.uid]: "wrong-parent-epoch" });
  check("qualified-wrong-parent", qualify, false);
  const pods = JSON.parse(run(["get", "pods", "-n", name, "-o", "json"])).items;
  assert(pods.every(p => !p.spec.nodeName && p.spec.automountServiceAccountToken === false), "Fixture unexpectedly scheduled or received identity");
  console.log(JSON.stringify({ nativeCases: results.length, passed: true, readinessClaimed: false }));
} catch (error) {
  primaryError = error;
  throw error;
} finally {
  const failures = [];
  for (const item of owned.reverse()) {
    try {
      const current = get(item.kind, item.name, item.namespace);
      assert.equal(current.metadata.uid, item.uid, "Fixture cleanup identity changed");
      const path = item.kind === "Namespace" ? `/api/v1/namespaces/${item.name}`
        : item.kind === "Deployment" ? `/apis/apps/v1/namespaces/${item.namespace}/deployments/${item.name}`
          : item.kind.startsWith("Validating") ? `/apis/admissionregistration.k8s.io/v1/${item.kind === "ValidatingAdmissionPolicy" ? "validatingadmissionpolicies" : "validatingadmissionpolicybindings"}/${item.name}`
            : `/apis/rbac.authorization.k8s.io/v1/namespaces/${item.namespace}/${item.kind.toLowerCase()}s/${item.name}`;
      run(["delete", "--raw", path, "-f", "-"], { apiVersion: "v1", kind: "DeleteOptions",
        preconditions: { uid: item.uid, resourceVersion: current.metadata.resourceVersion }, propagationPolicy: "Background" });
    } catch (error) { failures.push({ kind: item.kind, message: String(error.stderr ?? error.message) }); }
  }
  if (failures.length) throw new AggregateError([...(primaryError ? [primaryError] : []),
    new Error(JSON.stringify({ cleanupFailures: failures }))], "Native proof cleanup failed");
}
