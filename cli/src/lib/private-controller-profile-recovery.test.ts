// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe,expect,it } from "vitest";
import { rootCertificates } from "node:tls";
import { fixture,rootPod } from "./private-activation-fixtures.js";
import { annotations,canonical,previewPrivateActivation,PRIVATE_PREFIX,stagePrivateActivation } from "./private-activation.js";
import { retirementState,startRetirement } from "./private-activation-retirement.js";
import { applyControllerProfileRecovery,previewControllerProfileRecovery } from "./private-controller-profile-recovery.js";
import { applyBudgetTlsRecovery,previewBudgetTlsRecovery } from "./private-budget-tls-recovery.js";

async function pendingFixture(budget=false) {
  const f=fixture();
  const templateAnnotations: Record<string,string>={};
  if(budget) {
    templateAnnotations["checksum/inference-budget-tls"]="a".repeat(64);
    Object.assign(f.deployment.spec.template.metadata,{ annotations: templateAnnotations });
    Object.assign(f.deployment.spec.template.spec.containers[0]!,{
      env: [
        { name: "KARS_INFERENCE_BUDGET_ENABLED",value: "true" },
        { name: "KARS_INFERENCE_BUDGET_TLS_SECRET",value: "budget-tls" },
        { name: "POD_NAMESPACE",valueFrom: { fieldRef: { fieldPath: "metadata.namespace" } } },
      ]
    });
    f.objects.set(f.key("secret","budget-tls","core"),{
      type: "kubernetes.io/tls",
      metadata: {
        name: "budget-tls",namespace: "core",uid: "tls",resourceVersion: "1",
        annotations: { "kars.azure.com/inference-budget-tls": "v1" }
      },
      data: { "tls.crt": Buffer.from(rootCertificates[0]!).toString("base64") },
    });
  }
  f.pods.set("core",[rootPod(f)]);
  const activation=await previewPrivateActivation(f.execute,"core",[],[],"core","kcm-certificate",[]);
  f.pods.set("kube-system",[{
    kind: "Pod",
    metadata: {
      name: "controller-manager",uid: "kcm",resourceVersion: "1",
      annotations: { "kubernetes.io/config.mirror": "mirror" }
    },
    spec: {
      containers: [{
        name: "kube-controller-manager",
        command: ["kube-controller-manager","--use-service-account-credentials=true"]
      }]
    },
  }]);
  const ns=f.objects.get(f.key("namespace","core"));
  const state=startRetirement(activation,f.deployment,undefined);
  state.captured={ "core-uid": ["old-root"] };
  state.exposedKeys=activation.root.budgetTls? [activation.root.budgetTls.keyDigest]:[];
  ns.metadata.annotations={
    ...annotations(activation,activation.namespaces[0]!,"Pending"),
    [`${PRIVATE_PREFIX}root-retirement`]: canonical(state)
  };
  ns.metadata.resourceVersion="2";
  f.calls.length=0;
  return { ...f,activation,ns,state,templateAnnotations };
}

async function retiredBudgetFixture() {
  const f=await pendingFixture(true);
  await applyControllerProfileRecovery(f.execute,await previewControllerProfileRecovery(f.execute,f.activation));
  const activation=await previewPrivateActivation(f.execute,"core",[],[],"core","service-accounts",[]);
  const state=retirementState(f.ns)!;
  state.phase="retired";
  state.baseline={ secretUid: "tls",resourceVersion: "1",keyDigest: state.exposedKeys[0]! };
  f.deployment.spec.replicas=0;
  f.pods.set("core",[]);
  f.ns.metadata.annotations[`${PRIVATE_PREFIX}root-retirement`]=canonical(state);
  f.calls.length=0;
  return { ...f,activation,state };
}

function rotateBudgetFixture(f: Awaited<ReturnType<typeof retiredBudgetFixture>>) {
  const secret=f.objects.get(f.key("secret","budget-tls","core"));
  secret.data["tls.crt"]=Buffer.from(rootCertificates[1]!).toString("base64");
  secret.metadata.resourceVersion="2";
  f.deployment.metadata.resourceVersion="4";
  f.templateAnnotations["checksum/inference-budget-tls"]="b".repeat(64);
}

describe("retired managed budget TLS checksum recovery",() => {
  it("accepts only a fresh key and checksum delta, preserving the complete retirement history",async () => {
    const f=await retiredBudgetFixture();
    const review=await previewBudgetTlsRecovery(f.execute,f.activation);
    expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
    rotateBudgetFixture(f);
    const deploymentBefore=canonical(f.deployment);
    await applyBudgetTlsRecovery(f.execute,review);
    const after=retirementState(f.ns)!;
    expect({ ...after,binding: f.state.binding }).toEqual(f.state);
    expect(after.binding).not.toBe(f.state.binding);
    expect(f.ns.metadata.annotations[`${PRIVATE_PREFIX}state`]).toBe("Pending");
    expect(f.ns.metadata.annotations[`${PRIVATE_PREFIX}epoch`]).toBeUndefined();
    expect(canonical(f.deployment)).toBe(deploymentBefore);
    expect(f.calls.filter(args => args[0]==="patch")).toHaveLength(1);
    await expect(applyBudgetTlsRecovery(f.execute,review)).rejects.toThrow();
    const next=await previewPrivateActivation(f.execute,"core",[],[],"core","service-accounts",[]);
    expect(next.root.replicaIntent).toBe(1);
    expect(next.root.templateDigest).not.toBe(review.activation.root.templateDigest);
  });

  it.each(["live-root","captured-pod","pausing","no-checksum"])("refuses preview before safe retirement: %s",async (change) => {
    const f=await retiredBudgetFixture();
    if(change==="live-root") f.deployment.spec.replicas=1;
    if(change==="captured-pod") f.pods.set("core",[rootPod(f)]);
    if(change==="pausing") {
      f.state.phase="pausing";
      delete f.state.baseline;
      f.ns.metadata.annotations[`${PRIVATE_PREFIX}root-retirement`]=canonical(f.state);
    }
    if(change==="no-checksum") delete f.templateAnnotations["checksum/inference-budget-tls"];
    await expect(previewBudgetTlsRecovery(f.execute,f.activation)).rejects.toThrow();
    expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
  });

  it.each(["image","other-checksum","live-root","new-pod","new-descendant","terminating-descendant","namespace-version","root-uid",
    "account-uid","controller-uid","secret-uid","old-key","old-secret-version","old-checksum",
    "altered-original-checksum","unknown-field","writer"])("refuses changed %s without mutation",async (change) => {
      const f=await retiredBudgetFixture();
      const review=await previewBudgetTlsRecovery(f.execute,f.activation);
      rotateBudgetFixture(f);
      const secret=f.objects.get(f.key("secret","budget-tls","core"));
      if(change==="image") f.deployment.spec.template.spec.containers[0]!.image="different";
      if(change==="other-checksum") f.templateAnnotations["checksum/inference-credentials"]="c".repeat(64);
      if(change==="live-root") f.deployment.spec.replicas=1;
      if(change==="new-pod") f.pods.set("core",[rootPod(f,"new-root")]);
      if(change==="new-descendant"||change==="terminating-descendant") {
        const pod=rootPod(f,"new-root");
        Object.assign(pod.spec,{ serviceAccountName: "default",automountServiceAccountToken: false });
        if(change==="terminating-descendant") Object.assign(pod.metadata,{ deletionTimestamp: "2026-01-01T00:00:00Z" });
        f.pods.set("core",[pod]);
      }
      if(change==="namespace-version") f.ns.metadata.resourceVersion="999";
      if(change==="root-uid") f.deployment.metadata.uid="replacement";
      if(change==="account-uid") f.objects.get(f.key("serviceaccount","kars-controller","core")).metadata.uid="replacement";
      if(change==="controller-uid") f.objects.get(f.key("serviceaccount","deployment-controller","kube-system")).metadata.uid="replacement";
      if(change==="secret-uid") secret.metadata.uid="replacement";
      if(change==="old-key") secret.data["tls.crt"]=Buffer.from(rootCertificates[0]!).toString("base64");
      if(change==="old-secret-version") secret.metadata.resourceVersion="1";
      if(change==="old-checksum") f.templateAnnotations["checksum/inference-budget-tls"]="a".repeat(64);
      if(change==="altered-original-checksum") review.checksum="c".repeat(64);
      if(change==="unknown-field") Object.assign(review,{ ignored: true });
      if(change==="writer") f.objects.set(f.key("karscredentialgrants.kars.azure.com","workspace","core"),
        { metadata: { name: "workspace",namespace: "core",uid: "grant",resourceVersion: "1" },spec: { writers: [{ name: "writer" }] } });
      await expect(applyBudgetTlsRecovery(f.execute,review)).rejects.toThrow();
      expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
    });
});

describe("initial Pending controller profile recovery",() => {
  it.each([false,true])("refuses a late changed-ServiceAccount root descendant, terminating=%s",async terminating => {
    const f=await pendingFixture();
    let reads=0;
    const execute=async (args: string[],input?: string) => {
      if(args[0]==="get"&&args[1]==="pods"&&args[args.indexOf("-n")+1]==="core"&&++reads===2) {
        const pod=rootPod(f,"new-root");
        Object.assign(pod.spec,{ serviceAccountName: "default",automountServiceAccountToken: false });
        if(terminating) Object.assign(pod.metadata,{ deletionTimestamp: "2026-01-01T00:00:00Z" });
        f.pods.set("core",[pod]);
      }
      return f.execute(args,input);
    };
    await expect(previewControllerProfileRecovery(execute,f.activation)).rejects.toThrow();
    expect(reads).toBe(2);
    expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
    expect(f.ns.metadata.annotations[`${PRIVATE_PREFIX}profile`]).toBe("kcm-certificate");
  });

  it("refuses a contradictory profile before fresh preview or activation can pause the root",async () => {
    const f=await pendingFixture();
    await expect(previewPrivateActivation(f.execute,"core",[],[],"core","kcm-certificate",[]))
      .rejects.toThrow("contradicts");
    await expect(stagePrivateActivation(f.execute,f.activation)).rejects.toThrow("contradicts");
    expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
    expect(f.deployment.spec.replicas).toBe(1);
  });

  it("previews without mutations and applies only reviewed Pending namespace metadata",async () => {
    const f=await pendingFixture();
    const review=await previewControllerProfileRecovery(f.execute,f.activation);
    expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
    const rootBefore=canonical(f.deployment);
    await applyControllerProfileRecovery(f.execute,review);
    expect(f.ns.metadata.annotations[`${PRIVATE_PREFIX}profile`]).toBe("service-accounts");
    expect(f.ns.metadata.annotations[`${PRIVATE_PREFIX}state`]).toBe("Pending");
    expect(f.ns.metadata.annotations[`${PRIVATE_PREFIX}epoch`]).toBeUndefined();
    const after=retirementState(f.ns)!;
    expect(after.binding).not.toBe(f.state.binding);
    expect({ ...after,binding: f.state.binding }).toEqual(f.state);
    expect(canonical(f.deployment)).toBe(rootBefore);
    expect(f.calls.filter(args => args[0]==="patch")).toHaveLength(1);
    expect(f.calls.find(args => args[0]==="patch")?.slice(0,3)).toEqual(["patch","namespace","core"]);
    await expect(applyControllerProfileRecovery(f.execute,review)).rejects.toThrow();
  });

  it.each(["retired","restoring","epoch","unknown-field","replacement","template","new-pod","replicas"])(
    "refuses %s without mutation",async (change) => {
      const f=await pendingFixture();
      if(change==="retired"||change==="restoring") {
        f.state.phase=change;
        f.ns.metadata.annotations[`${PRIVATE_PREFIX}root-retirement`]=canonical(f.state);
      }
      if(change==="epoch") f.ns.metadata.annotations[`${PRIVATE_PREFIX}epoch`]="e".repeat(64);
      if(change==="unknown-field") f.ns.metadata.annotations[`${PRIVATE_PREFIX}unexpected`]="value";
      if(change==="replacement") f.deployment.metadata.uid="replacement";
      if(change==="template") f.deployment.spec.template.spec.containers[0]!.image="other";
      if(change==="new-pod") f.pods.set("core",[rootPod(f,"new-root")]);
      if(change==="replicas") f.deployment.spec.replicas=0;
      await expect(previewControllerProfileRecovery(f.execute,f.activation)).rejects.toThrow();
      expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
    },
  );

  it.each(["false","absent","duplicate","not-static"])("refuses unverified controller use: %s",async (change) => {
    const f=await pendingFixture();
    const pod=f.pods.get("kube-system")![0];
    if(change==="false") pod.spec.containers[0].command[1]="--use-service-account-credentials=false";
    if(change==="absent") f.pods.set("kube-system",[]);
    if(change==="duplicate") pod.spec.containers[0].command.push("--use-service-account-credentials=false");
    if(change==="not-static") delete pod.metadata.annotations["kubernetes.io/config.mirror"];
    await expect(previewControllerProfileRecovery(f.execute,f.activation)).rejects.toThrow();
    expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
  });

  it.each(["writer","private-proof"])("refuses an existing %s",async (change) => {
    const f=await pendingFixture();
    f.objects.set(f.key("karscredentialgrants.kars.azure.com","workspace","core"),{
      metadata: { name: "workspace",namespace: "core",uid: "grant",resourceVersion: "1" },
      spec: {
        writers: change==="writer"? [{ name: "writer" }]:[],
        ...(change==="private-proof"? { privateActivation: f.activation }:{})
      },
    });
    await expect(previewControllerProfileRecovery(f.execute,f.activation)).rejects.toThrow("existing private grants");
    expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
  });

  it.each(["namespace","controller-uid","manager-template","extra-review-field"])(
    "refuses changed %s after review",async (change) => {
      const f=await pendingFixture();
      const review=await previewControllerProfileRecovery(f.execute,f.activation);
      if(change==="namespace") f.ns.metadata.resourceVersion="3";
      if(change==="controller-uid") f.objects.get(f.key("serviceaccount","deployment-controller","kube-system")).metadata.uid="new";
      if(change==="manager-template") f.pods.get("kube-system")![0].spec.containers[0].image="other";
      if(change==="extra-review-field") Object.assign(review,{ extra: true });
      await expect(applyControllerProfileRecovery(f.execute,review)).rejects.toThrow();
      expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
    },
  );

  it("does not retry a failed namespace CAS",async () => {
    const f=await pendingFixture();
    const review=await previewControllerProfileRecovery(f.execute,f.activation);
    let patches=0;
    await expect(applyControllerProfileRecovery(async (args,input) => {
      if(args[0]==="patch") { patches++; throw new Error("Conflict"); }
      return f.execute(args,input);
    },review)).rejects.toThrow("Conflict");
    expect(patches).toBe(1);
    expect(f.ns.metadata.annotations[`${PRIVATE_PREFIX}profile`]).toBe("kcm-certificate");
  });

  it("preserves the exposed budget-key history without reading private key bytes",async () => {
    const f=await pendingFixture(true);
    const review=await previewControllerProfileRecovery(f.execute,f.activation);
    await applyControllerProfileRecovery(f.execute,review);
    expect(retirementState(f.ns)!.exposedKeys).toEqual(f.state.exposedKeys);
    expect(retirementState(f.ns)!.baseline).toBeUndefined();
    expect(f.calls.filter(args => args[1]==="secret").every(args =>
      args[0]==="get"&&!args.includes("json")&&!args.some(arg => arg.includes("tls.key")))).toBe(true);
  });

  it.each(["certificate","secret","baseline"])("refuses changed budget %s",async (change) => {
    const f=await pendingFixture(true);
    const review=await previewControllerProfileRecovery(f.execute,f.activation);
    const secret=f.objects.get(f.key("secret","budget-tls","core"));
    if(change==="certificate") secret.data["tls.crt"]=Buffer.from(rootCertificates[1]!).toString("base64");
    if(change==="secret") secret.metadata.uid="replacement";
    if(change==="baseline") {
      f.ns.metadata.annotations[`${PRIVATE_PREFIX}root-retirement`]=canonical({
        ...f.state,baseline: { secretUid: "tls",resourceVersion: "1",keyDigest: f.state.exposedKeys[0] },
      });
    }
    await expect(applyControllerProfileRecovery(f.execute,review)).rejects.toThrow();
    expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
  });

  it("refuses incomplete inventory rather than treating it as no writers",async () => {
    const f=await pendingFixture();
    await expect(previewControllerProfileRecovery(async (args,input) => {
      if(args[1]==="karscredentialgrants.kars.azure.com") {
        return JSON.stringify({ metadata: { continue: "more" },items: [] });
      }
      return f.execute(args,input);
    },f.activation)).rejects.toThrow("inventory is incomplete");
    expect(f.calls.every(args => ["get","auth"].includes(args[0]!))).toBe(true);
  });

  it("requires operator authority before examining or changing cluster state",async () => {
    const f=await pendingFixture();
    const calls: string[][]=[];
    await expect(previewControllerProfileRecovery(async args => {
      calls.push(args); return "no";
    },f.activation)).rejects.toThrow("operator authority");
    expect(calls).toHaveLength(1);
  });
});
