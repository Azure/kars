// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it, vi } from "vitest";
import { KubernetesError, type KubernetesJson } from "./kubernetes-json.js";
import { reviewedMission, type ReviewedTask } from "./mission-reviewed.js";
import { readRetainedMissionInputs } from "./mission-input-custody.js";
import { missionObjective } from "./mission-store.js";
import { MAX_MISSION_ARTIFACT_BYTES, missionContract, parseMissionMessage, type MissionAssignment, type MissionContract, type MissionReply } from "./mission-protocol.js";
import { missionRecordOwner, missionResultMaps, packMissionAttempt, unpackMissionAttempt, type MissionConfigMap } from "./mission-record.js";
import { missionAttemptName, missionContentDigest, type MissionAttempt, type MissionCandidate } from "./mission-dispatcher.js";
import { MAX_MISSION_INPUT_BYTES, missionInputs, type MissionInput, type MissionInputReference } from "./mission-inputs.js";

interface Resource { metadata: ReviewedTask["metadata"]; data?: Record<string,string>; immutable?: boolean }
interface Grant { contract: MissionContract; inputs: MissionInputReference[]; authority: Record<string,string> }
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

  it("derives v5 peer locators independently for new sealed runs and keeps legacy grants unchanged", async () => {
    const h = setup();
    expect((await h.authorize(fixture.phaseName, structuredClone(fixture.phaseGrant)))?.contract.version).toBe(4);
    h.mutateState(state => { state.schema = "kars.reviewed-run/v2"; });
    const state = JSON.parse(h.resources[h.statePath].data!["state.json"]);
    const grant = structuredClone(fixture.phaseGrant);
    const stateName = grant.authority.stateName;
    const peers = { rootUid: state.rootUid, stateName, slot: grant.authority.slot,
      peers: state.plan.roles.flatMap((role: any, r: number) => role.phases.map((phase: any, p: number) => ({
        role: role.name, phase: phase.name, slot: `r${r}-p${p}`, agentName: `rp-${stateName.slice(14,34)}-r${r}-p${p}`,
      }))) };
    grant.contract = missionContract(grant.contract.reviewedPhase, undefined, grant.contract.requiredOutputs, peers);
    expect(await h.authorize(fixture.phaseName, grant)).toEqual({ contract: grant.contract });
    for (const changed of [ { ...peers, rootUid: "different-root" }, { ...peers, slot: "synthesis" },
      { ...peers, peers: [] }, { ...peers, peers: peers.peers.map((p: any) => ({ ...p, role: "different-role" })) },
      { ...peers, peers: peers.peers.map((p: any) => ({ ...p, phase: "different-phase" })) } ]) {
      const altered = structuredClone(grant);
      altered.contract.peerContext = changed;
      await expect(h.authorize(fixture.phaseName, altered)).rejects.toThrow();
    }
    await expect(h.authorize(fixture.phaseName, structuredClone(fixture.phaseGrant))).rejects.toThrow();
    h.mutateState(state => { state.schema = "kars.reviewed-run/v1"; });
    await expect(h.authorize(fixture.phaseName, grant)).rejects.toThrow();
  });

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
  it.each(["memory", "revision"])("delivers exact sealed Team %s after the source Task has been collected",async kind=>{
    const h=revisionFixture(4096,runtime);
    const root=h.resources[taskPath("report")] as ReviewedTask;
    const content="# Prior clinic decision\r\nKeep the complete evidence — 日本語 🌍\n";
    const input={name:kind === "revision" ? "revision-0" : "memory-0-document-0.md",source:{
      namespace:"default",taskName:"collected-run",taskUid:"collected-uid",
      sandboxUid:"old-sandbox",podUid:"old-pod",runNonce:"old-run",assignmentId:"old-assignment",
      agentDid:`did:mesh:${"a".repeat(32)}`,artifactName:"decision-log.md",
    },sha256:missionContentDigest(content),content};
    const raw=JSON.stringify([input]);
    const key=`kars.azure.com/reviewed-team-${kind}`;
    root.metadata.annotations![key]=raw;
    const map=h.resources[`/api/v1/namespaces/default/configmaps/${h.grant.authority.stateName}`];
    const state=JSON.parse(map.data!["state.json"]); state[kind === "revision" ? "teamRevision" : "teamMemory"]=raw; map.data!["state.json"]=JSON.stringify(state);
    h.grant.inputs=[{name:input.name,source:input.source,sha256:input.sha256}];
    h.grant.contract=missionContract(h.grant.contract.reviewedPhase,[input],h.grant.contract.requiredOutputs);
    const result=await h.authorize(h.phase.metadata.name,h.grant);
    expect(result?.inputArtifacts).toEqual([input]);
    expect(h.readInputs).not.toHaveBeenCalled();
    expect(h.reads).not.toContain(taskPath("collected-run"));
    const proofDirectory = kind === "memory" ? process.env.KARS_REVIEWED_MEMORY_FIXTURE_DIR : process.env.KARS_REVIEWED_REVISION_FIXTURE_DIR;
    if (proofDirectory) {
      mkdirSync(proofDirectory, {recursive:true});
      writeFileSync(join(proofDirectory, `${kind}-${runtime}.json`), JSON.stringify({
        resources:h.resources, phaseName:h.phase.metadata.name, grant:h.grant,
        expected:result, rootPath:taskPath("report"),
      }), {flag:"wx"});
    }
    root.metadata.annotations![key]=JSON.stringify([{...input,content:"changed",sha256:missionContentDigest("changed")}]);
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow(`${kind} binding changed`);
    root.metadata.annotations![key]=raw;
    h.grant.inputs[0].sha256=missionContentDigest("substituted reference");
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow("sealed run");
  });
  function sealedRevision(count = 4) {
    const h = revisionFixture(4096,runtime);
    const root = h.resources[taskPath("report")] as ReviewedTask;
    const map = h.resources[`/api/v1/namespaces/default/configmaps/${h.grant.authority.stateName}`];
    const inputs: MissionInput[] = Array.from({length:count},(_,index) => {
      const content = `Retained document ${index} — 日本語\r\n${"evidence ".repeat(1024)}`;
      return {name:`revision-${index}`,content,sha256:missionContentDigest(content),source:{
        namespace:"default",taskName:"collected-run",taskUid:"collected-uid",sandboxUid:"old-sandbox",
        podUid:"old-pod",runNonce:"old-run",assignmentId:"old-assignment",
        agentDid:`did:mesh:${"a".repeat(32)}`,artifactName:`document-${index}.md`,
      }};
    });
    const seal = () => {
      const raw = JSON.stringify(inputs);
      root.metadata.annotations!["kars.azure.com/reviewed-team-revision"] = raw;
      const state = JSON.parse(map.data!["state.json"]); state.teamRevision = raw; map.data!["state.json"] = JSON.stringify(state);
    };
    seal();
    h.grant.inputs = inputs.map(({content: _content,...ref}) => ref);
    h.grant.contract = missionContract(h.grant.contract.reviewedPhase,inputs,h.grant.contract.requiredOutputs);
    return {...h,root,map,inputs,seal};
  }
  function referencedRevision() {
    const h = revisionFixture(4096,runtime);
    const root = h.resources[taskPath("report")] as ReviewedTask;
    const map = h.resources[`/api/v1/namespaces/default/configmaps/${h.grant.authority.stateName}`];
    // Exact observed UTF-8 and JSON-escaping sizes, without retained source bodies.
    const groups: [string,number,number][][] = [
      [["benchmarks.json",6196,715],["test_server.py",14522,290],["app.js",5685,52],["index.html",3615,71],
        ["server.py",45629,1679],["README.md",12109,83],["execution-report.md",9141,68],["execution-results.json",9500,637]],
      [["benchmark-evidence.json",15944,1534],["benchmark-handoff.md",8654,71],["handoff.md",8654,71]],
      [["lifecycle-evidence.json",10911,1193],["lifecycle-handoff.md",6554,41],["handoff.md",6554,41]],
      [["README.md",2264,41],["server.py",45629,1679],["test_server.py",14522,290],["benchmarks.json",6196,715],
        ["index.html",3615,71],["app.js",5685,52],["handoff.md",4201,46]],
      [["kars-python-executions.json",11018,803],["qa_bootstrap.py",1031,21],["qa_integration.py",6672,92],
        ["qa_live.py",2080,35],["execution-results.json",4430,331],["execution-report.md",6387,40],["handoff.md",3899,28]],
    ];
    const inputs: MissionInput[] = [];
    const sources: {attempt: MissionAttempt; record: MissionConfigMap; artifacts: MissionConfigMap; output: MissionConfigMap}[] = [];
    groups.forEach((files,group) => {
      const candidate: MissionCandidate = {namespace:"default",taskName:`retained-producer-${group}`,taskUid:`producer-${group}-uid`,
        sandboxUid:`sandbox-${group}`,podUid:`pod-${group}`,runNonce:`run-${group}`,agentDid:`did:mesh:${"a".repeat(32)}`,
        dispatcherDid:`did:mesh:${"b".repeat(32)}`,agentName:`Prior producer ${group}`,content:"Retain useful source documents.",
        ...(runtime === "Hermes" ? {runtime:"Hermes" as const} : {})};
      const contract = missionContract();
      const assignment: MissionAssignment = {...contract,type:"mission:assign",taskName:candidate.taskName,taskUid:candidate.taskUid,
        sandboxUid:candidate.sandboxUid,podUid:candidate.podUid,runNonce:candidate.runNonce,agentDid:candidate.agentDid,
        dispatcherDid:candidate.dispatcherDid,bootId:`boot-${group}`,assignmentId:`assignment-${group}`,artifactFormat:"text-v1",content:candidate.content};
      const artifacts: Record<string,string> = {};
      if (group > 0) files.sort(([a],[b])=>a<b?-1:a>b?1:0);
      files.forEach(([artifactName,bytes,escaped],index) => {
        const content = "\n".repeat(escaped) + "x".repeat(bytes-escaped);
        artifacts[artifactName] = content;
        inputs.push({name:group === 0 ? `revision-${index}` : artifactName === "handoff.md" ? `revision-unaccepted-r${group-1}-p0.md`
          : `revision-unaccepted-r${group-1}-p0--${index}-${artifactName}`,content,sha256:missionContentDigest(content),source:{
          namespace:candidate.namespace,taskName:candidate.taskName,taskUid:candidate.taskUid,sandboxUid:candidate.sandboxUid,
          podUid:candidate.podUid,runNonce:candidate.runNonce,assignmentId:assignment.assignmentId,agentDid:candidate.agentDid,artifactName}});
      });
      const evidence = {model:"reviewed-model",rounds:1,usage:{promptTokens:3,completionTokens:2,totalTokens:5}};
      const reply: MissionReply = {...assignment,type:"mission:reply",status:"succeeded",output:"Retained exports attached.",artifacts,evidence};
      const stamp = "2026-11-03T01:00:00.000Z";
      const attempt: MissionAttempt = {version:1,candidate,assignment,contentDigest:missionContentDigest(candidate.content),ownerSession:"dispatcher-session",
        startedAt:stamp,updatedAt:stamp,phase:"succeeded",events:[{at:stamp,status:"succeeded",evidence}],reply};
      const record: MissionConfigMap = {apiVersion:"v1",kind:"ConfigMap",metadata:{namespace:"default",name:missionAttemptName(candidate),
        uid:`record-${group}`,resourceVersion:"1",ownerReferences:[missionRecordOwner(candidate)]},data:{"attempt.json":packMissionAttempt(attempt)}};
      const expected = missionResultMaps(attempt);
      const output = {...expected.output,immutable:true,metadata:{...expected.output.metadata,uid:`output-${group}`,resourceVersion:"1"}};
      const retainedArtifacts = {...expected.artifacts,immutable:true,metadata:{...expected.artifacts.metadata,uid:`artifacts-${group}`,resourceVersion:"1"}};
      for (const cm of [record,output,retainedArtifacts]) h.resources[`/api/v1/namespaces/default/configmaps/${cm.metadata.name}`] = cm;
      const sourceTask = {apiVersion:"kars.azure.com/v1alpha1",kind:"KarsTask",metadata:{name:candidate.taskName,namespace:"default",uid:candidate.taskUid,resourceVersion:"1"}};
      h.resources[taskPath(candidate.taskName)] = sourceTask;
      sources.push({attempt,record,output,artifacts:retainedArtifacts});
    });
    const refs = inputs.map(({content:_content,...reference})=>reference);
    const manifest: {schema: string; inputs: MissionInputReference[]; diagnosticRoles?: Array<{name: string; phases: string[]}>} = {schema:"kars.team-revision-inputs/v1",inputs:refs};
    const seal = () => {
      const raw = JSON.stringify(manifest);
      root.metadata.annotations!["kars.azure.com/reviewed-team-revision"] = raw;
      const state = JSON.parse(map.data!["state.json"]); state.teamRevision = raw; map.data!["state.json"] = JSON.stringify(state);
    };
    seal();
    h.grant.inputs = structuredClone(refs);
    h.grant.contract = missionContract(h.grant.contract.reviewedPhase,inputs,h.grant.contract.requiredOutputs);
    return {...h,root,map,inputs,manifest,seal,sources};
  }
  function routedRevision(group: number) {
    const h = referencedRevision();
    const state = JSON.parse(h.map.data!["state.json"]);
    h.manifest.diagnosticRoles = Array.from({length:4},(_,i)=>({
      name:i === group-1 ? state.plan.roles[0].name : `historical-role-${i}`,
      phases:[state.plan.roles[0].phases[0].name],
    }));
    h.seal();
    const original = h.sources[group].attempt;
    const candidate: MissionCandidate = {...original.candidate,taskName:h.phase.metadata.name,taskUid:h.phase.metadata.uid!,
      runNonce:h.phase.metadata.annotations!["kars.azure.com/run-requested"],content:h.complete,
      reviewedPhase:h.grant.contract.reviewedPhase,requiredOutputs:["handoff.md"],inputArtifacts:missionInputs(h.inputs)};
    const contract = missionContract(candidate.reviewedPhase,candidate.inputArtifacts,["handoff.md"]);
    const assignment: MissionAssignment = {...original.assignment,...contract,taskName:candidate.taskName,taskUid:candidate.taskUid,
      runNonce:candidate.runNonce,content:candidate.content,inputArtifacts:candidate.inputArtifacts};
    const evidence = {...original.reply!.evidence!,phase:{name:contract.reviewedPhase!.name,
      minToolCalls:contract.reviewedPhase!.minToolCalls,maxToolCalls:contract.reviewedPhase!.maxToolCalls,
      attemptedToolCalls:1,successfulToolCalls:1}};
    const {inputArtifacts:_inputs,content:_content,...replyTarget} = assignment;
    const reply: MissionReply = {...replyTarget,type:"mission:reply",status:"succeeded",output:"Fresh specialist exports.",
      artifacts:structuredClone(original.reply!.artifacts),evidence};
    const attempt: MissionAttempt = {...original,candidate,assignment,reply,contentDigest:missionContentDigest(candidate.content),
      events:[{at:original.updatedAt,status:"succeeded",evidence}]};
    const record: MissionConfigMap = {apiVersion:"v1",kind:"ConfigMap",metadata:{namespace:"default",name:missionAttemptName(candidate),
      uid:"current-record",resourceVersion:"1",ownerReferences:[missionRecordOwner(candidate)]},data:{"attempt.json":packMissionAttempt(attempt)}};
    const maps = missionResultMaps(attempt);
    const output = {...maps.output,immutable:true,metadata:{...maps.output.metadata,uid:"current-output",resourceVersion:"1"}};
    const artifacts = {...maps.artifacts,immutable:true,metadata:{...maps.artifacts.metadata,uid:"current-artifacts",resourceVersion:"1"}};
    for (const cm of [record,output,artifacts]) h.resources[`/api/v1/namespaces/default/configmaps/${cm.metadata.name}`] = cm;
    h.phase.metadata.annotations!["kars.azure.com/run-completed"] = candidate.runNonce;
    const fresh: MissionInput[] = Object.entries(reply.artifacts!).sort(([a],[b])=>a<b?-1:a>b?1:0).map(([name,content],index)=>({
      name:name === "handoff.md" ? "r0-p0.md" : `r0-p0--${index}-${name}`,content,sha256:missionContentDigest(content),
      source:{namespace:"default",taskName:candidate.taskName,taskUid:candidate.taskUid,sandboxUid:candidate.sandboxUid,
        podUid:candidate.podUid,runNonce:candidate.runNonce,assignmentId:assignment.assignmentId,agentDid:candidate.agentDid,artifactName:name},
    }));
    const selected = missionInputs([...h.inputs.filter(input=>!input.name.startsWith(`revision-unaccepted-r${group-1}-p0`)),...fresh]);
    const grant = structuredClone(fixture.synthesisGrant);
    grant.authority.stateName = h.grant.authority.stateName;
    grant.inputs = selected.map(({content:_content,...ref})=>ref);
    grant.contract = missionContract(grant.contract.reviewedPhase,selected,grant.contract.requiredOutputs);
    return {...h,grant,fresh,selected,current:{attempt,record,output,artifacts}};
  }
  it.each([1,2,3,4])("routes current role %i exports without losing historical delivery or provenance",async group=>{
    const h = routedRevision(group);
    const before = structuredClone(h.resources);
    const result = await h.authorize("report",h.grant);
    expect(result?.inputArtifacts).toEqual(h.selected);
    expect(result?.inputArtifacts).toHaveLength(28);
    for (const prior of h.inputs.slice(0,8)) expect(result?.inputArtifacts).toContainEqual(prior);
    expect(h.readInputs).toHaveBeenCalledTimes(2);
    expect(h.readInputs.mock.calls.map(([refs])=>(refs as unknown[]).length)).toEqual([28,h.fresh.length]);
    expect(h.manifest.inputs).toHaveLength(28);
    expect(h.resources).toEqual(before);
    expect(Buffer.byteLength(JSON.stringify(h.selected))).toBeLessThan(384*1024);
    if (group === 4) expect(h.fresh.find(input=>input.source.artifactName === "kars-python-executions.json")!.name).toMatch(/^r0-p0--/);
  });
  it.each(["missing-shadowed-record","shadowed-bytes","wrong-role","wrong-phase","missing-current-file","stale-pin",
    "routing-null","routing-duplicate","routing-coordinate","assembled-bytes"])("rejects %s during routed revision admission",async mutation=>{
    const h = routedRevision(3);
    if (mutation === "missing-shadowed-record") delete h.resources[`/api/v1/namespaces/default/configmaps/${h.sources[3].record.metadata.name}`];
    if (mutation === "shadowed-bytes") h.sources[3].artifacts.data["server.py"] = "changed historical bytes";
    if (mutation === "wrong-role") h.manifest.diagnosticRoles![2].name = "different-role";
    if (mutation === "wrong-phase") h.manifest.diagnosticRoles![2].phases[0] = "different-phase";
    if (mutation === "missing-current-file") delete h.current.artifacts.data["server.py"];
    if (mutation === "stale-pin") h.grant.inputs.find(input=>input.name.startsWith("r0-p0--"))!.sha256 = missionContentDigest("different");
    if (mutation === "routing-null") Object.assign(h.manifest,{diagnosticRoles:null});
    if (mutation === "routing-duplicate") h.manifest.diagnosticRoles![0].name = h.manifest.diagnosticRoles![1].name;
    if (mutation === "routing-coordinate") h.manifest.diagnosticRoles![0].phases.push("later-phase");
    if (mutation === "assembled-bytes") {
      const memory = [0,1].map(i=>{const content="x".repeat(65536);return {...h.inputs[0],name:`memory-${i}.md`,content,sha256:missionContentDigest(content)};});
      const raw = JSON.stringify(memory); h.root.metadata.annotations!["kars.azure.com/reviewed-team-memory"] = raw;
      const state = JSON.parse(h.map.data!["state.json"]); state.teamMemory = raw; h.map.data!["state.json"] = JSON.stringify(state);
      h.grant.inputs = [...memory.map(({content:_content,...ref})=>ref),...h.grant.inputs].sort((a,b)=>a.name<b.name?-1:a.name>b.name?1:0);
    }
    h.seal();
    await expect(h.authorize("report",h.grant)).rejects.toThrow();
    if (["missing-shadowed-record","shadowed-bytes","missing-current-file","stale-pin","assembled-bytes"].includes(mutation)) {
      expect(h.readInputs).toHaveBeenCalled();
      expect(h.readInputs.mock.calls[0][0]).toHaveLength(28);
    }
  });
  it("resolves all 28 historical documents through retained custody without the inline revision ceiling",async()=>{
    const h = referencedRevision();
    expect(h.inputs).toHaveLength(28);
    expect(h.inputs.reduce((bytes,input)=>bytes+Buffer.byteLength(input.content),0)).toBe(281297);
    expect(Buffer.byteLength(JSON.stringify(h.inputs))).toBeGreaterThan(192*1024);
    expect(Buffer.byteLength(JSON.stringify(h.inputs))).toBeLessThan(384*1024);
    expect(Buffer.byteLength(JSON.stringify(h.manifest))).toBeLessThan(192*1024);
    expect(JSON.stringify(h.manifest)).not.toContain('"content"');
    const before = structuredClone(h.resources);
    const result = await h.authorize(h.phase.metadata.name,h.grant);
    expect(result?.inputArtifacts).toEqual(missionInputs(h.inputs));
    expect(h.readInputs).toHaveBeenCalledTimes(1);
    expect(h.readInputs.mock.calls[0][0]).toHaveLength(28);
    expect(h.resources).toEqual(before);
    expect(result?.inputArtifacts?.filter(input=>input.source.artifactName === "server.py")).toHaveLength(2);
    expect(result?.inputArtifacts?.find(input=>input.source.artifactName === "kars-python-executions.json")?.name).toMatch(/^revision-unaccepted-r3-p0--/);
    const proofDirectory = process.env.KARS_REVIEWED_REFERENCED_REVISION_FIXTURE_DIR;
    if (proofDirectory) {
      mkdirSync(proofDirectory, {recursive:true});
      writeFileSync(join(proofDirectory, `referenced-revision-${runtime}.json`), JSON.stringify({
        resources:h.resources, phaseName:h.phase.metadata.name, grant:h.grant, expected:result,
      }), {flag:"wx"});
    }
  });
  it.each(["schema","extra-field","inline-content","empty","duplicate","alias","producer","namespace","missing-pin","changed-pin","state","annotation-bytes","count"])("rejects %s reference manifests before custody reads",async mutation=>{
    const h = referencedRevision();
    if (mutation === "schema") h.manifest.schema = "unknown";
    if (mutation === "extra-field") Object.assign(h.manifest,{accepted:true});
    if (mutation === "inline-content") Object.assign(h.manifest.inputs[0],{content:"injected"});
    if (mutation === "empty") h.manifest.inputs = [];
    if (mutation === "duplicate") h.manifest.inputs[9].name = h.manifest.inputs[8].name;
    if (mutation === "alias") h.manifest.inputs[8].name = "revision-unaccepted-r0-p0--";
    if (mutation === "producer") h.manifest.inputs[9].source.podUid = "replacement-pod";
    if (mutation === "namespace") h.manifest.inputs[8].source.namespace = "other";
    if (mutation === "missing-pin") h.grant.inputs.pop();
    if (mutation === "changed-pin") h.grant.inputs[8].sha256 = missionContentDigest("different");
    if (mutation === "annotation-bytes") h.manifest.schema = "x".repeat(192*1024);
    if (mutation === "count") h.manifest.inputs = Array.from({length:153},(_,i)=>({...h.manifest.inputs[8],name:`revision-unaccepted-r${i}-p0.md`}));
    h.seal();
    if (mutation === "state") { const state = JSON.parse(h.map.data!["state.json"]); delete state.teamRevision; h.map.data!["state.json"] = JSON.stringify(state); }
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow();
    expect(h.readInputs).not.toHaveBeenCalled();
  });
  it.each(["missing-task","task-uid","record-owner","record","output","artifact","file-bytes","serialized-bytes","assembled-bytes"])("rejects %s in resolved historical documents rather than truncating",async mutation=>{
    const h = referencedRevision();
    const source = h.sources[1];
    const input = h.inputs[8];
    if (mutation === "missing-task") delete h.resources[taskPath(input.source.taskName)];
    if (mutation === "task-uid") h.resources[taskPath(input.source.taskName)].metadata.uid = "replacement";
    if (mutation === "record-owner") source.record.metadata.ownerReferences![0].uid = "replacement";
    if (mutation === "record") source.record.data["attempt.json"] = "{}";
    if (mutation === "output") source.output.data.output = "changed";
    if (mutation === "artifact") source.artifacts.data[input.source.artifactName] = "changed";
    if (mutation === "file-bytes" || mutation === "serialized-bytes") {
      for (const [index,document] of h.inputs.entries()) {
        if (mutation === "file-bytes" && index !== 8) continue;
        document.content = mutation === "file-bytes" ? "x".repeat(65537) : '"'.repeat(7800);
        document.sha256 = missionContentDigest(document.content);
        h.manifest.inputs[index].sha256 = document.sha256; h.grant.inputs[index].sha256 = document.sha256;
        h.sources.find(s=>s.attempt.candidate.taskName === document.source.taskName)!.attempt.reply!.artifacts![document.source.artifactName] = document.content;
      }
      if (mutation === "serialized-bytes") {
        expect(Buffer.byteLength(JSON.stringify(h.inputs))).toBeGreaterThan(MAX_MISSION_INPUT_BYTES);
        for (const producer of h.sources) {
          expect(Buffer.byteLength(JSON.stringify(producer.attempt.reply!.artifacts))).toBeLessThanOrEqual(MAX_MISSION_ARTIFACT_BYTES);
        }
      }
      for (const producer of h.sources) {
        producer.record.data["attempt.json"] = packMissionAttempt(producer.attempt);
        const canonical = missionResultMaps(producer.attempt);
        producer.artifacts.data = canonical.artifacts.data; producer.output.data = canonical.output.data;
      }
      h.seal();
    }
    if (mutation === "assembled-bytes") {
      const memory = [0,1].map(i=>{const content="x".repeat(65536);return {...h.inputs[0],name:`memory-${i}.md`,content,sha256:missionContentDigest(content)};});
      const raw = JSON.stringify(memory); h.root.metadata.annotations!["kars.azure.com/reviewed-team-memory"] = raw;
      const state = JSON.parse(h.map.data!["state.json"]); state.teamMemory = raw; h.map.data!["state.json"] = JSON.stringify(state);
      h.grant.inputs.push(...memory.map(({content:_content,...ref})=>ref));
    }
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow();
    expect(h.readInputs).toHaveBeenCalledTimes(1);
  });
  it("keeps the sealed revision limited to one producer's sixteen documents",async()=>{
    const h = sealedRevision(17);
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow("excessive inline revision documents");
    expect(h.readInputs).not.toHaveBeenCalled();
  });
  it("admits the full 143430-byte approved bundle with current peer context and unchanged limits",async()=>{
    const h = sealedRevision(7);
    const sizes = [29571,13205,19474,43332,17511,5728,4869];
    h.inputs.forEach((input,index) => {
      input.name = `memory-0-document-${index}.md`;
      input.content = "x".repeat(sizes[index]);
      input.sha256 = missionContentDigest(input.content);
    });
    h.inputs[0].content += "x".repeat(143430 - Buffer.byteLength(JSON.stringify(h.inputs)));
    h.inputs[0].sha256 = missionContentDigest(h.inputs[0].content);
    const raw = JSON.stringify(h.inputs);
    expect(Buffer.byteLength(raw)).toBe(143430);
    delete h.root.metadata.annotations!["kars.azure.com/reviewed-team-revision"];
    h.root.metadata.annotations!["kars.azure.com/reviewed-team-memory"] = raw;
    const state = JSON.parse(h.map.data!["state.json"]);
    delete state.teamRevision;
    state.teamMemory = raw;
    state.schema = "kars.reviewed-run/v2";
    h.map.data!["state.json"] = JSON.stringify(state);
    const peers = {rootUid:state.rootUid,stateName:h.grant.authority.stateName,slot:h.grant.authority.slot,
      peers:state.plan.roles.flatMap((role:any,r:number)=>role.phases.map((phase:any,p:number)=>({
        role:role.name,phase:phase.name,slot:`r${r}-p${p}`,agentName:`rp-${h.grant.authority.stateName.slice(14,34)}-r${r}-p${p}`,
      })))};
    h.grant.inputs = h.inputs.map(({content: _content,...ref}) => ref);
    h.grant.contract = missionContract(h.grant.contract.reviewedPhase,h.inputs,h.grant.contract.requiredOutputs,peers);
    const expected = {contract:h.grant.contract,inputArtifacts:missionInputs(h.inputs)};
    expect(await h.authorize(h.phase.metadata.name,h.grant)).toEqual(expected);
    expect(h.readInputs).not.toHaveBeenCalled();
    if (process.env.KARS_REVIEWED_MEMORY_FIXTURE_DIR) {
      writeFileSync(join(process.env.KARS_REVIEWED_MEMORY_FIXTURE_DIR, `memory-large-${runtime}.json`), JSON.stringify({
        resources:h.resources,phaseName:h.phase.metadata.name,grant:h.grant,expected,rootPath:taskPath("report"),
      }), {flag:"wx"});
    }
    const oversized = raw + " ".repeat(192 * 1024 - Buffer.byteLength(raw) + 1);
    h.root.metadata.annotations!["kars.azure.com/reviewed-team-memory"] = oversized;
    state.teamMemory = oversized;
    h.map.data!["state.json"] = JSON.stringify(state);
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow();
  });
  it.each([4,16])("retains every byte and source binding of %i sealed revision files, including numeric aliases",async count=>{
    const h = sealedRevision(count);
    expect((await h.authorize(h.phase.metadata.name,h.grant))?.inputArtifacts).toEqual(missionInputs(h.inputs));
    expect(h.readInputs).not.toHaveBeenCalled();
    expect(h.reads).not.toContain(taskPath("collected-run"));
  });
  it("binds retained revisions to current v5 peer context without allowing downgrade",async()=>{
    const h = sealedRevision();
    const state = JSON.parse(h.map.data!["state.json"]);
    state.schema = "kars.reviewed-run/v2";
    h.map.data!["state.json"] = JSON.stringify(state);
    const legacy = structuredClone(h.grant);
    const peers = {rootUid:state.rootUid,stateName:h.grant.authority.stateName,slot:h.grant.authority.slot,
      peers:state.plan.roles.flatMap((role:any,r:number)=>role.phases.map((phase:any,p:number)=>({
        role:role.name,phase:phase.name,slot:`r${r}-p${p}`,agentName:`rp-${h.grant.authority.stateName.slice(14,34)}-r${r}-p${p}`,
      })))};
    h.grant.contract = missionContract(h.grant.contract.reviewedPhase,h.inputs,h.grant.contract.requiredOutputs,peers);
    const expected = {contract:h.grant.contract,inputArtifacts:missionInputs(h.inputs)};
    expect(await h.authorize(h.phase.metadata.name,h.grant)).toEqual(expected);
    if (process.env.KARS_REVIEWED_REVISION_FIXTURE_DIR) {
      mkdirSync(process.env.KARS_REVIEWED_REVISION_FIXTURE_DIR, {recursive:true});
      writeFileSync(join(process.env.KARS_REVIEWED_REVISION_FIXTURE_DIR, `revision-v5-${runtime}.json`), JSON.stringify({
        resources:h.resources,phaseName:h.phase.metadata.name,grant:h.grant,expected,rootPath:taskPath("report"),
      }), {flag:"wx"});
    }
    await expect(h.authorize(h.phase.metadata.name,legacy)).rejects.toThrow("requirements differ");
  });
  it("combines retained memory and revision inputs without confusing their custody",async()=>{
    const h = sealedRevision();
    const memory = {...h.inputs[0],name:"memory-0-document-0.md",source:{...h.inputs[0].source,taskUid:"another-memory-run"}};
    const raw = JSON.stringify([memory]);
    h.root.metadata.annotations!["kars.azure.com/reviewed-team-memory"] = raw;
    const state = JSON.parse(h.map.data!["state.json"]); state.teamMemory = raw; h.map.data!["state.json"] = JSON.stringify(state);
    const inputs = [memory,...h.inputs];
    h.grant.inputs = inputs.map(({content: _content,...ref}) => ref);
    h.grant.contract = missionContract(h.grant.contract.reviewedPhase,inputs,h.grant.contract.requiredOutputs);
    expect((await h.authorize(h.phase.metadata.name,h.grant))?.inputArtifacts).toEqual(missionInputs(inputs));
    expect(h.readInputs).not.toHaveBeenCalled();
  });
  it.each(["alias","order","namespace","producer","hash","empty","file-size","total-size","count","missing-pin","extra-pin","changed-pin","owner","state"])("rejects %s revision corruption before dependency reads",async mutation=>{
    const h = sealedRevision();
    if (mutation === "alias") h.inputs[0].name = "revision-unbound";
    if (mutation === "order") h.inputs.reverse();
    if (mutation === "namespace") h.inputs.forEach(input => { input.source.namespace = "other-tenant"; });
    if (mutation === "producer") h.inputs[1].source.assignmentId = "different-assignment";
    if (mutation === "hash") h.inputs[0].sha256 = missionContentDigest("substituted");
    if (["empty","file-size","total-size"].includes(mutation)) {
      for (const input of h.inputs) {
        input.content = mutation === "empty" ? "" : "x".repeat(mutation === "file-size" ? 65537 : 65536);
        input.sha256 = missionContentDigest(input.content);
      }
    }
    if (mutation === "count") h.inputs.push(...Array.from({length:13},(_,index)=>({...h.inputs[0],name:`revision-${index+4}`})));
    h.seal();
    if (mutation === "missing-pin") h.grant.inputs.pop();
    if (mutation === "extra-pin") h.grant.inputs.push({...h.grant.inputs[0],name:"revision-4"});
    if (mutation === "changed-pin") h.grant.inputs[0].sha256 = missionContentDigest("different pin");
    if (mutation === "owner") delete h.root.metadata.ownerReferences;
    if (mutation === "state") {
      const state = JSON.parse(h.map.data!["state.json"]); delete state.teamRevision; h.map.data!["state.json"] = JSON.stringify(state);
    }
    await expect(h.authorize(h.phase.metadata.name,h.grant)).rejects.toThrow();
    expect(h.readInputs).not.toHaveBeenCalled();
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

  function nativePlan() {
    const h=setup();
    const state=JSON.parse(h.resources[h.statePath].data!["state.json"]);
    state.plan.roles[0].phases[0].executionMode="native";
    state.plan.roles[0].phases[0].capabilities=["filesystem-write","network"];
    state.plan.synthesis.executionMode="native";
    state.plan.deliverables[0].mediaType="text/csv";
    const seal=()=>{
      const sorted=(value:unknown):unknown=>Array.isArray(value) ? value.map(sorted)
        : value !== null && typeof value === "object" ? Object.fromEntries(Object.entries(value).sort(([a],[b])=>a < b ? -1 : a > b ? 1 : 0).map(([k,v])=>[k,sorted(v)])) : value;
      state.planDigest=missionContentDigest(JSON.stringify(sorted(state.plan)));
      (h.resources[taskPath("report")] as ReviewedTask).spec.blueprint!.executionPlan=structuredClone(state.plan);
      h.resources[h.statePath].data!["state.json"]=JSON.stringify(state);
    };
    seal();
    const grant=structuredClone(fixture.phaseGrant);
    grant.contract=missionContract(state.plan.roles[0].phases[0],undefined,grant.contract.requiredOutputs);
    return {...h,state,seal,grant};
  }

  function singleAgentPlan() {
    const h=nativePlan();
    h.state.plan.roles=[];
    h.state.children={};
    h.state.plan.synthesis.capabilities=["filesystem-write","network"];
    const root=h.resources[taskPath("report")] as ReviewedTask;
    root.spec.envelope!.delegationDepth=0;
    h.seal();
    const grant=structuredClone(fixture.synthesisGrant);
    grant.inputs=[];
    grant.contract=missionContract({name:"synthesis",...h.state.plan.synthesis,minToolCalls:0,freshContext:true},undefined,grant.contract.requiredOutputs);
    return {...h,grant};
  }

  it("admits an explicitly reviewed native single agent without children or delegation authority",async()=>{
    const h=singleAgentPlan();
    expect(await h.authorize("report",h.grant)).toEqual({contract:h.grant.contract});
    expect(h.readInputs).not.toHaveBeenCalled();
    expect(h.reads.filter(path=>path.includes("/karstasks/")).every(path=>path===taskPath("report"))).toBe(true);
    h.grant.contract=missionContract({...h.grant.contract.reviewedPhase!,capabilities:["filesystem-read","filesystem-write","network"]},undefined,h.grant.contract.requiredOutputs);
    await expect(h.authorize("report",h.grant)).rejects.toThrow("requirements differ");
  });

  it.each(["legacy","parallel","zero-bound","missing-write","unsupported","child-reservation"])("rejects single-agent %s without inventing authority",async fault=>{
    const h=singleAgentPlan();
    if(fault==="legacy") delete h.state.plan.synthesis.executionMode;
    if(fault==="parallel") h.state.plan.maxParallel=2;
    if(fault==="zero-bound") h.state.plan.synthesis.maxToolCalls=0;
    if(fault==="missing-write") h.state.plan.synthesis.capabilities=["network"];
    if(fault==="unsupported") h.state.plan.synthesis.capabilities.push("shell");
    if(fault==="child-reservation") h.state.children.r0={name:"unreviewed"};
    h.seal();
    await expect(h.authorize("report",h.grant)).rejects.toThrow();
    expect(h.readInputs).not.toHaveBeenCalled();
  });

  it("retains explicit github-pr in single-agent synthesis but never grants it to decomposed synthesis",async()=>{
    const h=singleAgentPlan();
    h.state.plan.synthesis.capabilities.push("github-pr");
    h.seal();
    h.grant.contract=missionContract({name:"synthesis",...h.state.plan.synthesis,minToolCalls:0,freshContext:true},undefined,h.grant.contract.requiredOutputs);
    expect(await h.authorize("report",h.grant)).toEqual({contract:h.grant.contract});
    const decomposed=nativePlan();
    decomposed.state.plan.synthesis.capabilities.push("github-pr");
    decomposed.seal();
    await expect(decomposed.authorize(fixture.phaseName,decomposed.grant)).rejects.toThrow("unsupported synthesis");
  });

  it("admits explicitly reviewed github-pr in a member phase and rejects an unbound grant",async()=>{
    const h=nativePlan();
    h.state.plan.roles[0].phases[0].capabilities.push("github-pr");
    h.seal();
    await expect(h.authorize(fixture.phaseName,h.grant)).rejects.toThrow("requirements differ");
    h.grant.contract=missionContract(h.state.plan.roles[0].phases[0],undefined,h.grant.contract.requiredOutputs);
    expect(await h.authorize(fixture.phaseName,h.grant)).toEqual({contract:h.grant.contract});
  });

  it("admits native research without unused read permission and preserves exact native authority",async()=>{
    const h=nativePlan();
    expect(await h.authorize(fixture.phaseName,h.grant)).toEqual({contract:h.grant.contract});
    expect(h.readInputs).not.toHaveBeenCalled();
    const changed={...h.grant.contract.reviewedPhase!,capabilities:["filesystem-read","filesystem-write","network"]};
    h.grant.contract=missionContract(changed,undefined,h.grant.contract.requiredOutputs);
    await expect(h.authorize(fixture.phaseName,h.grant)).rejects.toThrow("requirements differ");
  });

  it("admits native synthesis with controller-supported CSV metadata and immutable prior handoffs",async()=>{
    const h=nativePlan();
    const retained=JSON.parse(fixture.resources[h.statePath].data!["state.json"]);
    h.state.plan.roles=retained.plan.roles;
    h.seal();
    const inputs=await h.readInputs(fixture.synthesisGrant.inputs);
    const grant=structuredClone(fixture.synthesisGrant);
    grant.contract=missionContract({...grant.contract.reviewedPhase!,executionMode:"native"},inputs,grant.contract.requiredOutputs);
    const result=await h.authorize("report",grant);
    expect(result?.contract).toEqual(grant.contract);
    expect(result?.inputArtifacts).toEqual(inputs);
    expect(result?.contract.reviewedPhase?.executionMode).toBe("native");
  });

  it("admits the first native role when the sealed synthesis includes governed memory",async()=>{
    const h=nativePlan();
    h.state.plan.synthesis.capabilities.push("memory");
    h.seal();
    expect(await h.authorize(fixture.phaseName,h.grant)).toEqual({contract:h.grant.contract});
    expect(h.readInputs).not.toHaveBeenCalled();
  });

  it("preserves explicitly reviewed native synthesis memory and rejects unbound capability changes",async()=>{
    const h=nativePlan();
    h.state.plan.roles=JSON.parse(fixture.resources[h.statePath].data!["state.json"]).plan.roles;
    h.state.plan.synthesis.capabilities.push("memory");
    h.seal();
    const inputs=await h.readInputs(fixture.synthesisGrant.inputs);
    const grant=structuredClone(fixture.synthesisGrant);
    grant.contract=missionContract({name:"synthesis",...h.state.plan.synthesis,minToolCalls:0,freshContext:true},inputs,grant.contract.requiredOutputs);
    const result=await h.authorize("report",grant);
    expect(result?.contract).toEqual(grant.contract);
    expect(result?.inputArtifacts).toEqual(inputs);
    grant.contract=missionContract({...grant.contract.reviewedPhase!,capabilities:["filesystem-read","filesystem-write"]},inputs,grant.contract.requiredOutputs);
    await expect(h.authorize("report",grant)).rejects.toThrow("requirements differ");
  });

  it.each(["legacy-memory","network","missing-read","missing-write","duplicate-memory"])("rejects unsupported Team synthesis %s",async fault=>{
    const h=nativePlan();
    h.state.plan.synthesis.capabilities.push("memory");
    if(fault==="legacy-memory") delete h.state.plan.synthesis.executionMode;
    if(fault==="network") h.state.plan.synthesis.capabilities.push("network");
    if(fault==="missing-read") h.state.plan.synthesis.capabilities=["filesystem-write","memory"];
    if(fault==="missing-write") h.state.plan.synthesis.capabilities=["filesystem-read","memory"];
    if(fault==="duplicate-memory") h.state.plan.synthesis.capabilities.push("memory");
    h.seal();
    await expect(h.authorize(fixture.phaseName,h.grant)).rejects.toThrow();
    expect(h.readInputs).not.toHaveBeenCalled();
  });

  it.each(["missing-write","dependent-read","later-read","synthesis-read","synthesis-write","unknown-mode","unknown-media"])("rejects native plan with %s without widening authority",async fault=>{
    const h=nativePlan();
    const phase=h.state.plan.roles[0].phases[0];
    if(fault==="missing-write") phase.capabilities=["network"];
    if(fault==="dependent-read") {
      h.state.plan.roles.push({...structuredClone(h.state.plan.roles[0]),name:"dependent",dependsOn:[h.state.plan.roles[0].name]});
    }
    if(fault==="later-read") h.state.plan.roles[0].phases.push({...structuredClone(phase),name:"later"});
    if(fault==="synthesis-read") h.state.plan.synthesis.capabilities=["filesystem-write","network"];
    if(fault==="synthesis-write") h.state.plan.synthesis.capabilities=["filesystem-read","network"];
    if(fault==="unknown-mode") h.state.plan.synthesis.executionMode="other";
    if(fault==="unknown-media") h.state.plan.deliverables[0].mediaType="application/octet-stream";
    h.seal();
    await expect(h.authorize(fixture.phaseName,h.grant)).rejects.toThrow();
    expect(h.readInputs).not.toHaveBeenCalled();
  });

  it("retains exact required fetch authority through sealed plan admission",async()=>{
    const h=setup();
    const root=h.resources[taskPath("report")] as ReviewedTask;
    const state=JSON.parse(h.resources[h.statePath].data!["state.json"]);
    const phase=state.plan.roles[0].phases[0];
    phase.capabilities.push("network");
    phase.requiredToolCalls=[{name:"http_fetch",arguments:{method:"GET",url:"https://www.example.com/pricing"}}];
    const sorted=(value: unknown): unknown => Array.isArray(value) ? value.map(sorted)
      : value !== null && typeof value === "object" ? Object.fromEntries(Object.entries(value).sort(([a],[b])=>a < b ? -1 : a > b ? 1 : 0).map(([k,v])=>[k,sorted(v)])) : value;
    state.planDigest=missionContentDigest(JSON.stringify(sorted(state.plan)));
    root.spec.blueprint!.executionPlan=structuredClone(state.plan);
    h.resources[h.statePath].data!["state.json"]=JSON.stringify(state);
    const grant=structuredClone(fixture.phaseGrant);
    grant.contract=missionContract(phase,undefined,grant.contract.requiredOutputs);
    expect(await h.authorize(fixture.phaseName,grant)).toEqual({contract:grant.contract});
    const changed=structuredClone(phase);
    changed.requiredToolCalls[0].arguments.url="https://www.example.com/unreviewed";
    grant.contract=missionContract(changed,undefined,grant.contract.requiredOutputs);
    await expect(h.authorize(fixture.phaseName,grant)).rejects.toThrow("requirements differ");
    delete phase.requiredToolCalls;
    phase.capabilities=phase.capabilities.filter((c:string)=>c!=="network");
    grant.contract=missionContract(phase,undefined,grant.contract.requiredOutputs);
    await expect(h.authorize(fixture.phaseName,grant)).rejects.toThrow("requirements differ");
  });

  it("admits actual Rust-projected draft files, handoff and truncated aliases with exact immutable custody",async()=>{
    const h=setup();
    expect(await h.authorize(fixture.phaseName,fixture.phaseGrant)).toEqual({contract:fixture.phaseGrant.contract});
    expect(h.readInputs).not.toHaveBeenCalled();
    const result=await h.authorize();
    expect(result?.contract).toEqual(fixture.synthesisGrant.contract);
    expect(result?.inputArtifacts).toHaveLength(4);
    expect(result?.inputArtifacts?.find(input=>input.name==="r0-p0.md")?.content).toBe("Reviewed handoff — 日本語");
    expect(result?.inputArtifacts?.find(input=>input.source.artifactName==="station-plan.csv")?.content).toBe('station,jobs,notes\n1,14,"quoted, note"\n');
    expect(result?.inputArtifacts?.find(input=>input.source.artifactName.length===128)?.name).toHaveLength(128);
    expect(result?.contract.requiredOutputs).toEqual(["report.md"]);
    expect(missionContract(result?.contract.reviewedPhase,result?.inputArtifacts,result?.contract.requiredOutputs)).toEqual(result?.contract);
  });
  it("retains compatibility with a previously projected handoff-only grant",async()=>{
    const h=setup(); const g=structuredClone(fixture.synthesisGrant);
    g.inputs=g.inputs.filter(input=>input.source.artifactName==="handoff.md");
    const inputs=await h.readInputs(g.inputs);
    g.contract=missionContract(g.contract.reviewedPhase,inputs,g.contract.requiredOutputs);
    expect((await h.authorize("report",g))?.inputArtifacts).toEqual(inputs);
  });
  it.each(["taskUid","taskName","namespace","runNonce","sandboxUid","podUid","assignmentId","agentDid","alias","duplicate","missing-handoff","generated"])("rejects exported-document %s substitution before custody",async fault=>{
    const h=setup(); const g=structuredClone(fixture.synthesisGrant);
    const document=g.inputs.find(input=>input.source.artifactName==="station-plan.csv")!;
    if(fault==="alias") document.name="unreviewed.csv";
    else if(fault==="duplicate") g.inputs.push({...structuredClone(document),name:"duplicate.csv"});
    else if(fault==="missing-handoff") g.inputs=g.inputs.filter(input=>input.source.artifactName!=="handoff.md");
    else if(fault==="generated") document.source.artifactName="response.md";
    else (document.source as unknown as Record<string,string>)[fault]="unreviewed";
    await expect(h.authorize("report",g)).rejects.toThrow();
    expect(h.readInputs).not.toHaveBeenCalled();
  });
  it("rejects exported bytes that differ from the controller-pinned digest",async()=>{
    const h=setup(); const g=structuredClone(fixture.synthesisGrant);
    g.inputs.find(input=>input.source.artifactName==="station-plan.csv")!.sha256=missionContentDigest("substituted CSV");
    await expect(h.authorize("report",g)).rejects.toThrow();
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
