// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Disposable Kind API/CEL preflight. No controller/router image build is needed.
// Credentials stay in this process: never persist or print TokenRequest bodies.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const require = createRequire(new URL("../../cli/package.json", import.meta.url));
const { parseAllDocuments } = require("yaml");
const root = fileURLToPath(new URL("../../", import.meta.url));
const context = "kind-kars-budget-api";
const namespace = "budget-api-fixture";
const controller = `system:serviceaccount:${namespace}:kars-controller`;
const principal = `system:serviceaccount:${namespace}:untrusted`;
const audience = "kars.azure.com/governed-inference-budget";
const shared = JSON.parse(readFileSync(new URL("../../deploy/helm/kars/files/inference-budget-admission.json", import.meta.url), "utf8")
  .replaceAll("__ACCOUNTING_NAMESPACE__", namespace));

function kubectl(args, input, publicSchema = false) {
  try {
    return execFileSync("kubectl", ["--context", context, "--request-timeout=20s", ...args], {
      cwd: root, encoding: "utf8", input: input === undefined ? undefined : JSON.stringify(input),
      stdio: ["pipe", "pipe", "pipe"], timeout: 30_000,
    });
  } catch (error) {
    if (publicSchema) {
      // This opt-in is used ONLY for the four public CRDs and eight public VAPs
      // below. Do not enable it for Secret/token/agent-response commands.
      console.error(String(error.stderr ?? "").slice(0, 12_000));
    }
    throw new Error("Disposable budget API assertion command failed", { cause: undefined });
  }
}

function create(value, as, publicSchema = false) {
  return JSON.parse(kubectl(["create", "-f", "-", "-o", "json", ...(as ? ["--as", as] : [])], value, publicSchema));
}

function denied(value, as) {
  assert.throws(() => create(value, as), /Disposable budget API assertion command failed/);
}

async function until(check, description) {
  for (let attempt = 0; attempt < 60; attempt++) {
    if (await check()) return;
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  throw new Error(`Timed out: ${description}`);
}

const clusterNodes = execFileSync("kind", ["get", "nodes", "--name", "kars-budget-api"], {
  encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 20_000,
}).trim().split(/\s+/);
assert(clusterNodes.length > 0 && clusterNodes.every((name) => name.startsWith("kars-budget-api-")));
const version = JSON.parse(kubectl(["get", "--raw", "/version"]));
assert.match(version.gitVersion, /^v1\.31\./, "Use the same pinned Kind v0.24/v1.31 apiserver as the supported harness");
create({ apiVersion: "v1", kind: "Namespace", metadata: { name: namespace } });

const rendered = execFileSync("helm", ["template", "budget-api", "deploy/helm/kars", "--namespace", namespace], {
  cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 30_000,
});
const crdNames = [
  "karssandboxes.kars.azure.com", "karstasks.kars.azure.com",
  "karsteams.kars.azure.com", "karsbudgetaccounts.kars.azure.com",
];
const definitions = parseAllDocuments(rendered).map((document) => {
  assert.equal(document.errors.length, 0);
  return document.toJSON();
}).filter((value) => value?.kind === "CustomResourceDefinition" && crdNames.includes(value.metadata.name));
assert.equal(definitions.length, crdNames.length);
for (const definition of definitions) {
  create(definition, undefined, true);
  kubectl(["wait", "--for=condition=Established", `crd/${definition.metadata.name}`, "--timeout=60s"]);
}
for (const policy of shared.items) {
  create({ apiVersion: "admissionregistration.k8s.io/v1", kind: "ValidatingAdmissionPolicy",
    metadata: { name: policy.name }, spec: policy.spec }, undefined, true);
  create({ apiVersion: "admissionregistration.k8s.io/v1", kind: "ValidatingAdmissionPolicyBinding",
    metadata: { name: policy.name }, spec: { policyName: policy.name, validationActions: ["Deny", "Audit"] } }, undefined, true);
  await until(() => {
    const observed = JSON.parse(kubectl(["get", "validatingadmissionpolicy", policy.name, "-o", "json"]));
    if (observed.status?.observedGeneration !== observed.metadata.generation) return false;
    const warnings = observed.status?.typeChecking?.expressionWarnings ?? [];
    assert.deepEqual(warnings, [], `Public CEL compilation warnings: ${JSON.stringify(warnings)}`);
    return true;
  }, `public policy compilation ${policy.name}`);
}

for (const name of ["kars-controller", "untrusted"]) {
  create({ apiVersion: "v1", kind: "ServiceAccount", metadata: { name, namespace } });
}
// Deliberately over-grant only these fixture principals in the disposable
// namespace: negative tests must prove admission, not merely missing RBAC.
create({ apiVersion: "rbac.authorization.k8s.io/v1", kind: "Role", metadata: { name: "fixture", namespace },
  rules: [
    { apiGroups: ["kars.azure.com"], resources: ["karsbudgetaccounts", "karsbudgetaccounts/status", "karstasks/status"], verbs: ["get", "create", "patch", "update"] },
    { apiGroups: [""], resources: ["serviceaccounts/token", "configmaps"], verbs: ["create", "get", "patch"] },
  ] });
create({ apiVersion: "rbac.authorization.k8s.io/v1", kind: "RoleBinding", metadata: { name: "fixture", namespace },
  roleRef: { apiGroup: "rbac.authorization.k8s.io", kind: "Role", name: "fixture" },
  subjects: ["kars-controller", "untrusted"].map((name) => ({ kind: "ServiceAccount", name, namespace })) });

const identity = { namespace, name: "root", uid: "immutable-root-uid" };
const account = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsBudgetAccount",
  metadata: { name: "inference-budget-root", namespace },
  spec: { scope: "GovernedInference", root: { kind: "KarsTask", resource: identity,
    workspaceUid: "workspace-uid", clusterUid: "cluster-uid" }, limits: { tokens: 100, usdMicros: 10 } } };
denied(account, principal);
const created = create(account, controller);
const changedRoot = structuredClone(created);
changedRoot.spec.root.resource.uid = "replacement-root-uid";
assert.throws(() => kubectl(["replace", "-f", "-", "--as", controller], changedRoot));
assert.equal(JSON.parse(kubectl(["get", "karsbudgetaccount", created.metadata.name, "-n", namespace, "-o", "json"])).metadata.uid, created.metadata.uid);

const taskPlan = {
  apiVersion:"kars.azure.com/v1alpha1", kind:"KarsTask",
  metadata:{name:"finite-task", namespace},
  spec:{objective:"Public API budget fixture", envelope:{
    tier:3, authorityCeiling:3, delegationDepth:2,
    budget:{scope:"GovernedInference", tokens:100, usdMicros:10},
  }, execution:{launch:true}},
};
const scopedTask = create(taskPlan);
assert.equal(scopedTask.spec.envelope.budget.scope, "GovernedInference");
const legacyTask = structuredClone(taskPlan);
legacyTask.metadata.name = "legacy-finite";
delete legacyTask.spec.envelope.budget.scope;
denied(legacyTask);
const unbounded = structuredClone(taskPlan);
unbounded.metadata.name = "existing-unbounded";
unbounded.spec.envelope.budget = {tokens:0, usdMicros:0};
const unboundedTask = create(unbounded);
unboundedTask.spec.envelope.budget = taskPlan.spec.envelope.budget;
assert.throws(() => kubectl(["replace", "-f", "-"], unboundedTask));
const changedScope = structuredClone(scopedTask);
delete changedScope.spec.envelope.budget.scope;
changedScope.spec.execution.launch = false;
assert.throws(() => kubectl(["replace", "-f", "-"], changedScope));

const tokenRequest = { apiVersion: "authentication.k8s.io/v1", kind: "TokenRequest",
  spec: { audiences: [audience], expirationSeconds: 600 } };
assert.throws(() => kubectl(["create", "--raw",
  `/api/v1/namespaces/${namespace}/serviceaccounts/untrusted/token`, "-f", "-", "--as", principal], tokenRequest));

const projection = { apiVersion: "v1", kind: "ConfigMap", metadata: { name: "kars-inference-budget-ca", namespace },
  data: { "ca.crt": "public-test-only" } };
denied(projection, principal);
create(projection, controller);
const regular = { ...projection, metadata: { name: "unrelated-customer-data", namespace } };
create(regular, principal);
assert.equal(JSON.parse(kubectl(["get", "configmap", regular.metadata.name, "-n", namespace, "-o", "json"])).data["ca.crt"], "public-test-only");

// Genuine kubelet issuance and TokenReview extra claims on the supported API.
// This fixture namespace is deliberately NOT a protected runtime namespace;
// production exec/attach into a finite runtime namespace is denied separately.
const pod = create({ apiVersion: "v1", kind: "Pod", metadata: { name: "identity", namespace },
  spec: { serviceAccountName: "untrusted", restartPolicy: "Never",
    containers: [{ name: "router", image: "busybox:latest", command: ["sleep", "300"],
      volumeMounts: [{ name: "audience", mountPath: "/private", readOnly: true }] }],
    volumes: [{ name: "audience", projected: { sources: [{ serviceAccountToken: {
      audience, expirationSeconds: 600, path: "token",
    } }] } }] } });
kubectl(["wait", "-n", namespace, "--for=condition=Ready", "pod/identity", "--timeout=90s"]);
const token = kubectl(["exec", "-n", namespace, "identity", "-c", "router", "--", "cat", "/private/token"]).trim();
assert(token.length > 0);
const review = create({ apiVersion: "authentication.k8s.io/v1", kind: "TokenReview",
  spec: { audiences: [audience], token } });
assert.equal(review.status.authenticated, true);
assert.deepEqual(review.status.audiences, [audience]);
assert.deepEqual(review.status.user.extra["authentication.kubernetes.io/pod-uid"], [pod.metadata.uid]);
assert.deepEqual(review.status.user.extra["authentication.kubernetes.io/pod-name"], ["identity"]);
console.log("Budget public CRDs/CEL, controller-only accounting/CA, private audience issuance and UID claims passed on disposable Kubernetes v1.31");
