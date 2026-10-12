// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it, vi } from "vitest";
import { KubernetesError, type KubernetesJson } from "./kubernetes-json.js";
import { reviewedMission, type ReviewedTask } from "./mission-reviewed.js";
import { readRetainedMissionInputs } from "./mission-input-custody.js";
import { missionObjective } from "./mission-store.js";
import { missionContract, parseMissionMessage, type MissionContract } from "./mission-protocol.js";
import { missionResultMaps, unpackMissionAttempt, type MissionConfigMap } from "./mission-record.js";
import { missionContentDigest } from "./mission-dispatcher.js";

interface Resource { metadata: ReviewedTask["metadata"]; data?: Record<string,string>; immutable?: boolean }
interface Grant { contract: MissionContract; inputs: Array<{name:string; source: {namespace:string;taskName:string;taskUid:string;runNonce:string};sha256:string}>; authority: Record<string,string> }
interface ReviewedFixture {
  resources: Record<string,Resource>; phaseName: string; phaseGrant: Grant; synthesisGrant: Grant;
}
const taskPath = (name: string) => `/apis/kars.azure.com/v1alpha1/namespaces/default/karstasks/${name}`;
describe.each([
  ["OpenClaw", "protocol-fixture.json"],
  ["Hermes", "protocol-hermes-fixture.json"],
])("%s controller reviewed authority and immutable custody", (runtime, filename) => {
const fixture = JSON.parse(readFileSync(new URL(`../../controller/src/reviewed_execution/${filename}`,import.meta.url),"utf8")) as ReviewedFixture;
function setup() {
  const resources = structuredClone(fixture.resources);
  const reads: string[] = [];
  const api: KubernetesJson = { async request<T>(method:string,path:string): Promise<T> {
    expect(method).toBe("GET"); reads.push(path);
    if (!resources[path]) throw new KubernetesError(404,method,path);
    return structuredClone(resources[path]) as T;
  } };
  const readInputs = vi.fn((refs:unknown) => readRetainedMissionInputs(refs,{
    task: source => api.request("GET",taskPath(source.taskName)),
    configMap: (ns,name) => api.request("GET",`/api/v1/namespaces/${ns}/configmaps/${name}`),
  }));
  const task = (name:string) => structuredClone(resources[taskPath(name)]) as ReviewedTask;
  const authorize = (name="report", grant=structuredClone(fixture.synthesisGrant)) => reviewedMission(api,task(name),grant,missionObjective,readInputs);
  const statePath = `/api/v1/namespaces/default/configmaps/${fixture.synthesisGrant.authority.stateName}`;
  const mutateState = (mutate:(state:Record<string,unknown>)=>void) => {
    const data=resources[statePath].data!; const state=JSON.parse(data["state.json"]); mutate(state); data["state.json"]=JSON.stringify(state);
  };
  return {resources,reads,api,task,readInputs,authorize,statePath,mutateState};
}

function revisionFixture(characters: number, teamRuntime?: string) {
  const h = setup();
  const prefix = "kars.azure.com/";
  const root = h.resources[taskPath("report")] as ReviewedTask;
  const stateMap = h.resources[h.statePath];
  const state = JSON.parse(stateMap.data!["state.json"]);
  const role = state.plan.roles[0];
  const roleAuthority = teamRuntime === undefined ? undefined : {
    blueprint: {runtime:teamRuntime,modelDeployment:"role-model",modelFallbacks:["role-fallback"],toolPolicy:"role-policy"},
    envelope:{...structuredClone(root.spec.envelope!),delegationDepth:1,budget:{scope:"GovernedInference",tokens:2000}},
  };
  if (roleAuthority) {
    root.metadata.ownerReferences=[{apiVersion:"kars.azure.com/v1alpha1",kind:"KarsTeam",name:"standing-team",uid:"team-uid",controller:true}];
    root.metadata.annotations![prefix+"team-request-state"]="admitted";
    root.metadata.annotations![prefix+"run-requested"]=root.metadata.name;
    root.metadata.annotations![prefix+"reviewed-team-roles"]=JSON.stringify({[role.name]:roleAuthority});
    root.spec.execution!.runtime=runtime;
    state.teamRoles=root.metadata.annotations![prefix+"reviewed-team-roles"];
  }
  const suffix = `\n\nRole: ${role.objective}\n\nPhase: ${role.phases[0].objective}`;
  const content = "é".repeat(characters - Array.from(suffix).length);
  const complete = content + suffix;
  const nonce = root.metadata.annotations![prefix + "run-requested"];
  root.metadata.annotations![prefix + "run-objective-nonce"] = nonce;
  root.metadata.annotations![prefix + "run-objective-b64"] = Buffer.from(content).toString("base64");
  root.metadata.annotations![prefix + "run-objective-digest"] = missionContentDigest(content);
  state.admission.objectiveDigest = missionContentDigest(content);
  state.admission.runNonce=nonce;
  const stateName = `kars-reviewed-${missionContentDigest(JSON.stringify([
    root.metadata.uid, root.metadata.generation, root.status!.envelopeDigest, nonce, state.admission.objectiveDigest,
  ])).slice(7,47)}`;
  const pin = JSON.stringify({name:stateName,uid:stateMap.metadata.uid});
  root.metadata.annotations![prefix + "reviewed-state"] = pin;
  let parent = root;
  for (const slot of ["r0","r0-p0"]) {
    const reservation = state.children[slot];
    const child = h.resources[taskPath(reservation.name)] as ReviewedTask;
    delete h.resources[taskPath(reservation.name)];
    child.metadata.name = `rp-${stateName.slice(14,34)}-${slot}`;
    child.spec.parentRef = {name:parent.metadata.name};
    child.metadata.ownerReferences![0].name = parent.metadata.name;
    child.metadata.annotations![prefix + "reviewed-state"] = pin;
    child.metadata.annotations![prefix + "run-requested"] = `plan-${stateName.slice(14)}-${slot}`;
    if (roleAuthority) {
      child.spec.blueprint=structuredClone(roleAuthority.blueprint);
      child.spec.envelope=structuredClone(roleAuthority.envelope);
      child.spec.envelope.delegationDepth=slot==="r0" ? 1 : 0;
      child.spec.execution={launch:slot==="r0-p0"};
    }
    if (slot === "r0-p0") {
      child.spec.objective = characters > 4096 ? suffix.slice(2) : complete;
      if (characters > 4096) {
        child.metadata.annotations![prefix + "run-objective-nonce"] = child.metadata.annotations![prefix + "run-requested"];
        child.metadata.annotations![prefix + "run-objective-b64"] = Buffer.from(complete).toString("base64");
        child.metadata.annotations![prefix + "run-objective-digest"] = missionContentDigest(complete);
      }
    }
    reservation.name = child.metadata.name;
    reservation.spec = structuredClone(child.spec);
    h.resources[taskPath(child.metadata.name)] = child;
    parent = child;
  }
  delete h.resources[h.statePath];
  stateMap.metadata.name = stateName;
  stateMap.data!["state.json"] = JSON.stringify(state);
  h.resources[`/api/v1/namespaces/default/configmaps/${stateName}`] = stateMap;
  const grant = structuredClone(fixture.phaseGrant);
  grant.authority.stateName = stateName;
  return {...h,phase:parent,grant,complete};
}

  it.each([4096,4097,24000])("admits complete revision context at %i Unicode characters without widening the Task schema",async characters=>{
    const h=revisionFixture(characters);
    expect(Array.from(h.phase.spec.objective).length).toBeLessThanOrEqual(4096);
    expect(missionObjective(h.phase,h.phase.metadata.annotations!["kars.azure.com/run-requested"])).toBe(h.complete);
    expect(await h.authorize(h.phase.metadata.name,h.grant)).toEqual({contract:h.grant.contract});
  });
  it.each(["nonce","b64","digest","coherent"])("rejects altered long-phase %s objective bindings",async field=>{
    const h=revisionFixture(24000);
    const annotations=h.phase.metadata.annotations!;
    if(field==="coherent") {
      annotations["kars.azure.com/run-objective-b64"]=Buffer.from("different authorized-looking instructions").toString("base64");
      annotations["kars.azure.com/run-objective-digest"]=missionContentDigest("different authorized-looking instructions");
    } else annotations[`kars.azure.com/run-objective-${field}`]="changed";
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow("objective binding changed");
  });
  it("rejects added objective annotations on an otherwise unchanged short phase",async()=>{
    const h=revisionFixture(4096);
    h.phase.metadata.annotations!["kars.azure.com/run-objective-nonce"]=h.phase.metadata.annotations!["kars.azure.com/run-requested"];
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow("objective binding changed");
  });

  it("admits the sealed opposite-runtime Team role with its model, policy and tighter budget",async()=>{
    const selected=runtime==="OpenClaw" ? "Hermes" : "OpenClaw";
    const h=revisionFixture(4096,selected);
    expect(h.phase.spec.blueprint).toEqual({runtime:selected,modelDeployment:"role-model",modelFallbacks:["role-fallback"],toolPolicy:"role-policy"});
    expect(h.phase.spec.envelope?.budget?.tokens).toBe(2000);
    expect(h.phase.spec.execution?.runtime).toBeUndefined();
    expect(await h.authorize(h.phase.metadata.name,h.grant)).toEqual({contract:h.grant.contract});
  });
  it("delivers exact sealed Team memory after the source Task has been collected",async()=>{
    const h=revisionFixture(4096,runtime);
    const root=h.resources[taskPath("report")] as ReviewedTask;
    const content="# Prior clinic decision\r\nKeep the complete evidence — 日本語 🌍\n";
    const input={name:"memory-0-document-0.md",source:{
      namespace:"default",taskName:"collected-run",taskUid:"collected-uid",
      sandboxUid:"old-sandbox",podUid:"old-pod",runNonce:"old-run",assignmentId:"old-assignment",
      agentDid:`did:mesh:${"a".repeat(32)}`,artifactName:"decision-log.md",
    },sha256:missionContentDigest(content),content};
    const raw=JSON.stringify([input]);
    root.metadata.annotations!["kars.azure.com/reviewed-team-memory"]=raw;
    const map=h.resources[`/api/v1/namespaces/default/configmaps/${h.grant.authority.stateName}`];
    const state=JSON.parse(map.data!["state.json"]); state.teamMemory=raw; map.data!["state.json"]=JSON.stringify(state);
    h.grant.inputs=[{name:input.name,source:input.source,sha256:input.sha256}];
    h.grant.contract=missionContract(h.grant.contract.reviewedPhase,[input],h.grant.contract.requiredOutputs);
    const result=await h.authorize(h.phase.metadata.name,h.grant);
    expect(result?.inputArtifacts).toEqual([input]);
    expect(h.readInputs).not.toHaveBeenCalled();
    expect(h.reads).not.toContain(taskPath("collected-run"));
    const proofDirectory = process.env.KARS_REVIEWED_MEMORY_FIXTURE_DIR;
    if (proofDirectory) {
      mkdirSync(proofDirectory, {recursive:true});
      writeFileSync(join(proofDirectory, `memory-${runtime}.json`), JSON.stringify({
        resources:h.resources, phaseName:h.phase.metadata.name, grant:h.grant,
        expected:result, rootPath:taskPath("report"),
      }), {flag:"wx"});
    }
    root.metadata.annotations!["kars.azure.com/reviewed-team-memory"]=JSON.stringify([{...input,content:"changed",sha256:missionContentDigest("changed")}]);
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow("memory binding changed");
    root.metadata.annotations!["kars.azure.com/reviewed-team-memory"]=raw;
    h.grant.inputs[0].sha256=missionContentDigest("substituted reference");
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow("sealed run");
  });
  it.each(["state","root","missing","owner","phase-runtime","phase-policy","phase-budget"])("rejects changed Team %s authority",async field=>{
    const h=revisionFixture(4096,runtime==="OpenClaw" ? "Hermes" : "OpenClaw");
    const root=h.resources[taskPath("report")] as ReviewedTask;
    const key="kars.azure.com/reviewed-team-roles";
    if(field==="state") {
      const map=h.resources[`/api/v1/namespaces/default/configmaps/${h.grant.authority.stateName}`];
      const state=JSON.parse(map.data!["state.json"]); state.teamRoles="{}"; map.data!["state.json"]=JSON.stringify(state);
    }
    if(field==="root") root.metadata.annotations![key]="{}";
    if(field==="missing") delete root.metadata.annotations![key];
    if(field==="owner") delete root.metadata.ownerReferences;
    if(field==="phase-runtime") h.phase.spec.execution!.runtime=runtime;
    if(field==="phase-policy") h.phase.spec.blueprint!.toolPolicy="different-policy";
    if(field==="phase-budget") h.phase.spec.envelope!.budget!.tokens=5000;
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow();
    expect(h.readInputs).not.toHaveBeenCalled();
  });

  it("admits the exact controller phase and synthesizes only its immutable named handoff",async()=>{
    const h=setup();
    expect(await h.authorize(fixture.phaseName,fixture.phaseGrant)).toEqual({contract:fixture.phaseGrant.contract});
    expect(h.readInputs).not.toHaveBeenCalled();
    const result=await h.authorize();
    expect(result?.contract).toEqual(fixture.synthesisGrant.contract);
    expect(result?.inputArtifacts).toHaveLength(1);
    expect(result?.inputArtifacts?.[0].content).toBe("Reviewed handoff — 日本語");
    expect(result?.contract.requiredOutputs).toEqual(["report.md"]);
    expect(missionContract(result?.contract.reviewedPhase,result?.inputArtifacts,result?.contract.requiredOutputs)).toEqual(result?.contract);
  });
  it("round-trips actual Rust durable evidence through strict SDK parsing and projections",()=>{
    const record=Object.values(fixture.resources).find(r=>r.data?.["attempt.json"]) as MissionConfigMap;
    const input=fixture.synthesisGrant.inputs[0];
    const {attempt}=unpackMissionAttempt(record,input.source);
    expect(attempt.candidate.runtime ?? "OpenClaw").toBe(runtime);
    expect(attempt.assignment.activityFormat).toBe(runtime === "OpenClaw" ? "router-observations-v1" : undefined);
    expect(attempt.reply?.activityFormat).toBe(attempt.assignment.activityFormat);
    expect(parseMissionMessage(attempt.assignment)).not.toBeNull();
    expect(parseMissionMessage(attempt.reply)).not.toBeNull();
    const expected=missionResultMaps(attempt);
    for(const map of [expected.output,expected.artifacts]) {
      expect(fixture.resources[`/api/v1/namespaces/default/configmaps/${map.metadata.name}`].data).toEqual(map.data);
    }
  });
  it.each(["taskName","taskUid","runNonce","namespace"])("rejects graph-unauthorized source %s before custody",async field=>{
    const h=setup(); const g=structuredClone(fixture.synthesisGrant);
    (g.inputs[0].source as Record<string,string>)[field]="unreviewed";
    await expect(h.authorize("report",g)).rejects.toThrow();
    expect(h.readInputs).not.toHaveBeenCalled();
  });
  it.each(["report",fixture.phaseName,fixture.phaseName.replace(/-p0$/,"")])("rejects a non-Ready root, role or source %s before custody",async name=>{
    const h=setup(); const t=h.resources[taskPath(name)] as ReviewedTask; t.status!.phase="Pending";
    await expect(h.authorize()).rejects.toThrow("Ready"); expect(h.readInputs).not.toHaveBeenCalled();
  });
  it.each(["uid","spec","nonce","rebind","pin"])("rejects reserved source %s changes before custody",async kind=>{
    const h=setup(); const t=h.resources[taskPath(fixture.phaseName)] as ReviewedTask;
    if(kind==="uid") t.metadata.uid="replacement";
    if(kind==="spec") t.spec.objective="Other objective after the review";
    if(kind==="nonce") t.metadata.annotations!["kars.azure.com/run-requested"]="other";
    if(kind==="rebind") t.metadata.annotations!["kars.azure.com/credential-rebind-pending"]="true";
    if(kind==="pin") t.metadata.annotations!["kars.azure.com/reviewed-state"]="{";
    await expect(h.authorize()).rejects.toThrow(); expect(h.readInputs).not.toHaveBeenCalled();
  });
  it.each(["immutable","projection","failed","incomplete"])("rejects %s source evidence",async kind=>{
    const h=setup();
    if(kind==="incomplete") delete h.resources[taskPath(fixture.phaseName)].metadata.annotations!["kars.azure.com/run-completed"];
    else if(kind==="failed") {
      const record=Object.values(h.resources).find(r=>r.data?.["attempt.json"])!;
      const attempt=JSON.parse(record.data!["attempt.json"]); attempt.phase="failed"; record.data!["attempt.json"]=JSON.stringify(attempt);
    } else {
      const artifacts=Object.values(h.resources).find(r=>r.data?.["handoff.md"]=== "Reviewed handoff — 日本語")!;
      if(kind==="immutable") artifacts.immutable=false;
      else artifacts.data!["handoff.md"]="forged successful handoff";
    }
    await expect(h.authorize()).rejects.toThrow();
  });
  it.each(["shell","freshContext","requiredToolCalls","maxToolCalls","budgetTokens"])("keeps unsupported full-plan %s closed",async kind=>{
    const h=setup(); const root=h.resources[taskPath("report")] as ReviewedTask;
    const plan=root.spec.blueprint!.executionPlan as {roles:Array<{budgetTokens?:number;phases:Array<Record<string,unknown>>}>};
    const phase=plan.roles[0].phases[0];
    if(kind==="shell") phase.capabilities=["filesystem-read","filesystem-write","shell"];
    if(kind==="freshContext") phase.freshContext=false;
    if(kind==="requiredToolCalls") phase.requiredToolCalls=[{tool:"github_actions_job_logs",arguments:{}}];
    if(kind==="maxToolCalls") phase.maxToolCalls=0;
    if(kind==="budgetTokens") plan.roles[0].budgetTokens=10001;
    await expect(h.authorize()).rejects.toThrow(); expect(h.readInputs).not.toHaveBeenCalled();
  });
  it.each(["contract","authority","inputs","state"])("rejects installed %s disagreement",async kind=>{
    const h=setup();const g=structuredClone(fixture.synthesisGrant);
    if(kind==="contract") g.contract.requiredOutputs=["different.md"];
    if(kind==="authority") g.authority.slot="r0-p0";
    if(kind==="inputs") g.inputs=[];
    if(kind==="state") h.resources[h.statePath].metadata.uid="replacement";
    await expect(h.authorize("report",g)).rejects.toThrow();
  });
  it("fences authority replaced during immutable custody reads",async()=>{
    const h=setup(); const original=h.readInputs.getMockImplementation()!;
    h.readInputs.mockImplementation(async refs=>{
      const inputs=await original(refs);h.resources[taskPath("report")].metadata.resourceVersion="changed";return inputs;
    });
    await expect(h.authorize()).rejects.toThrow("changed during custody");
  });
  it("rejects unknown persisted reservation coordinates",async()=>{
    const h=setup();h.mutateState(s=>{(s.children as Record<string,unknown>).r9={};});
    await expect(h.authorize()).rejects.toThrow();expect(h.readInputs).not.toHaveBeenCalled();
  });
});
