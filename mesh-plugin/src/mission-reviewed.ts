// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import type { KubernetesJson } from "./kubernetes-json.js";
import { missionContract, missionRequiredOutputs, parseMissionContract, sameMissionContract, type MissionContract } from "./mission-protocol.js";
import { filesystemPhase, type FilesystemPhase } from "./mission-phase.js";
import { MAX_MISSION_RETAINED_INPUT_BYTES, missionInputReferences, missionInputs, type MissionInput, type MissionInputReference } from "./mission-inputs.js";
import type { MissionMetadata } from "./mission-workload.js";
import { missionRuntime } from "./mission-runtime.js";

export interface ReviewedTask {
  apiVersion: string; kind: string; metadata: MissionMetadata;
  spec: { objective: string; execution?: { launch?: boolean; runtime?: string }; parentRef?: { name: string };
    displayName?: string; envelope?: { tier: number; authorityCeiling: number; delegationDepth?: number;
      budget?: { scope?: string; tokens?: number; usdMicros?: number } };
    blueprint?: { executionPlan?: unknown; runtime?: string; [key: string]: unknown } };
  status?: { phase?: string; envelopeDigest?: string; observedGeneration?: number; conditions?: Array<{type: string; status: string}> };
}
interface Role { name: string; objective: string; dependsOn?: string[]; budgetTokens?: number; phases: FilesystemPhase[] }
interface Plan { schema: string; roles: Role[]; maxParallel: number; synthesis: { objective: string; capabilities: string[]; maxToolCalls: number; executionMode?: "native" }; deliverables?: Array<{name: string; mediaType?: string}> }
interface Reservation { name: string; uid: string; spec: ReviewedTask["spec"] }
interface State { schema: string; rootName: string; rootUid: string; admission: unknown; planDigest: string; plan: Plan; teamRoles?: string; teamMemory?: string; teamRevision?: string; children: Record<string, Reservation> }
interface TeamRoleAuthority { blueprint: ReviewedTask["spec"]["blueprint"] | null; envelope: NonNullable<ReviewedTask["spec"]["envelope"]> }
interface Map { metadata: MissionMetadata; data?: Record<string,string> }
const prefix = "kars.azure.com/";
const pinKey = `${prefix}reviewed-state`;
const rootKey = `${prefix}reviewed-root`;
const slotKey = `${prefix}reviewed-slot`;
const runKey = `${prefix}run-requested`;
const teamRolesKey = `${prefix}reviewed-team-roles`;
const teamMemoryKey = `${prefix}reviewed-team-memory`;
const teamRevisionKey = `${prefix}reviewed-team-revision`;
const hash = (text: string): string => `sha256:${createHash("sha256").update(text).digest("hex")}`;
const sorted = (v: unknown): unknown => Array.isArray(v) ? v.map(sorted) : v && typeof v === "object"
  ? Object.fromEntries(Object.entries(v).sort(([a],[b]) => a < b ? -1 : a > b ? 1 : 0).map(([k,v]) => [k,sorted(v)])) : v;
const same = (a: unknown,b: unknown): boolean => a === undefined || b === undefined ? a === b : isDeepStrictEqual(JSON.parse(JSON.stringify(a)),JSON.parse(JSON.stringify(b)));
export class ReviewedExecutionError extends Error {}
export class ReviewedSnapshotChangedError extends ReviewedExecutionError {}
const requireFact = (condition: unknown, message: string): void => { if (!condition) throw new ReviewedExecutionError(`Reviewed execution: ${message}`); };
const segment = (name: string): string => {
  requireFact(typeof name === "string" && /^[a-z0-9][a-z0-9.-]{0,252}$/.test(name) && name !== "." && name !== "..", "invalid resource name");
  return encodeURIComponent(name);
};
const taskPath = (ns: string,name: string): string => `/apis/kars.azure.com/v1alpha1/namespaces/${segment(ns)}/karstasks/${segment(name)}`;
const mapPath = (ns: string,name: string): string => `/api/v1/namespaces/${segment(ns)}/configmaps/${segment(name)}`;
const slot = (r: number,p?: number): string => p === undefined ? `r${r}` : `r${r}-p${p}`;
function owned(resource: {metadata: MissionMetadata}, task: ReviewedTask): void {
  requireFact(resource.metadata.uid && resource.metadata.resourceVersion && !resource.metadata.deletionTimestamp
    && resource.metadata.namespace === task.metadata.namespace && same(resource.metadata.ownerReferences, [{
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", name: task.metadata.name, uid: task.metadata.uid, controller: true,
    }]), "resource is outside exact Task custody");
}
function shape(value: unknown, keys: readonly string[]): void {
  requireFact(value && typeof value === "object" && !Array.isArray(value)
    && Object.keys(value).every(key => keys.includes(key)), "invalid object shape");
}
function boundedJson(value: string | undefined, maximum: number): unknown {
  requireFact(typeof value === "string" && Buffer.byteLength(value) <= maximum, "missing or oversized authority");
  try { return JSON.parse(value!); } catch { throw new ReviewedExecutionError("Reviewed execution: malformed authority"); }
}
function currentReady(task: ReviewedTask): void {
  requireFact(task.apiVersion === "kars.azure.com/v1alpha1" && task.kind === "KarsTask" && task.metadata.uid && task.metadata.resourceVersion
    && !task.metadata.deletionTimestamp && Number.isSafeInteger(task.metadata.generation) && task.metadata.generation! > 0
    && task.metadata.generation === task.status?.observedGeneration && task.status?.phase === "Ready"
    && /^sha256:[a-f0-9]{64}$/.test(task.status?.envelopeDigest ?? "")
    && task.status?.conditions?.some(c => c.type === "Ready" && c.status === "True")
    && task.metadata.annotations?.[`${prefix}credential-rebind-pending`] !== "true", "Task is not current and Ready");
}
function normalizePlan(input: unknown): Plan {
  const p = structuredClone(input) as Plan;
  shape(p,["schema","roles","maxParallel","synthesis","deliverables"]);
  requireFact(p.schema === "kars.execution-plan/v1" && Array.isArray(p.roles) && p.roles.length <= 8
    && (p.roles.length > 0 || p.synthesis?.executionMode === "native")
    && Number.isSafeInteger(p.maxParallel) && p.maxParallel >= 1 && p.maxParallel <= Math.max(1,p.roles.length), "unsupported plan");
  for (const r of p.roles) {
    shape(r,["name","objective","dependsOn","budgetTokens","phases"]);
    requireFact(typeof r.name === "string" && /^[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?$/.test(r.name)
      && typeof r.objective === "string" && Buffer.byteLength(r.objective.trim()) >= 20 && Buffer.byteLength(r.objective) <= 1200
      && (r.budgetTokens === undefined || (Number.isSafeInteger(r.budgetTokens) && r.budgetTokens > 0)), "invalid reviewed role");
  }
  const names = new Set(p.roles.map(r => r.name));
  requireFact(names.size === p.roles.length, "duplicate roles");
  for (const r of p.roles) {
    requireFact(Array.isArray(r.phases) && r.phases.length >= 1 && r.phases.length <= 8, "invalid phases");
    if (r.dependsOn === undefined) r.dependsOn = [];
    requireFact(Array.isArray(r.dependsOn) && r.dependsOn.length <= p.roles.length && new Set(r.dependsOn).size === r.dependsOn.length
      && r.dependsOn.every(name => names.has(name) && name !== r.name), "invalid dependency");
    const phases = new Set<string>();
    for (const [index, phase] of r.phases.entries()) {
      const normalized = filesystemPhase(phase);
      const needsRead = normalized.executionMode !== "native" || index > 0 || r.dependsOn.length > 0;
      requireFact(normalized.freshContext && normalized.maxToolCalls > 0
        && (!needsRead || normalized.capabilities.includes("filesystem-read"))
        && normalized.capabilities.includes("filesystem-write") && !phases.has(normalized.name), "unsupported phase");
      phases.add(normalized.name);
      // Rust omits empty requiredToolCalls; canonical wire phases always include them.
      phase.minToolCalls ??= 0;
      if (!normalized.requiredToolCalls.length) delete (phase as Partial<FilesystemPhase>).requiredToolCalls;
    }
  }
  const done = new Set<string>();
  while (done.size < p.roles.length) {
    const next = p.roles.filter(r => !done.has(r.name) && r.dependsOn!.every(name => done.has(name)));
    requireFact(next.length > 0, "cyclic dependencies");
    next.forEach(r => done.add(r.name));
  }
  shape(p.synthesis,["objective","capabilities","maxToolCalls","executionMode"]);
  const synthesis = filesystemPhase({name:"synthesis",...p.synthesis,minToolCalls:0,freshContext:true});
  const synthesisFiles = synthesis.capabilities.filter(capability => !(synthesis.executionMode === "native" && capability === "memory"));
  requireFact(synthesis.maxToolCalls > 0 && (p.roles.length === 0
    ? synthesis.executionMode === "native" && (!(p.deliverables?.length) || synthesis.capabilities.includes("filesystem-write"))
    : synthesisFiles.length === 2 && synthesisFiles.includes("filesystem-read")
      && synthesisFiles.includes("filesystem-write")), "unsupported synthesis");
  if (p.deliverables === undefined) p.deliverables = [];
  requireFact(Array.isArray(p.deliverables) && p.deliverables.length <= 16, "invalid deliverables");
  for (const d of p.deliverables) {
    shape(d,["name","mediaType"]);
    requireFact(d.mediaType === undefined || ["text/plain","text/markdown","text/csv","application/json"].includes(d.mediaType), "unsupported output media type");
  }
  missionRequiredOutputs(p.deliverables.map(d => d.name));
  return p;
}
export function needsReviewedAuthority(task: ReviewedTask): boolean {
  return task.spec.blueprint?.executionPlan !== undefined || task.metadata.annotations?.[rootKey] !== undefined
    || task.metadata.annotations?.[slotKey] !== undefined || task.metadata.annotations?.[pinKey] !== undefined;
}

function teamRoleAuthorities(root: ReviewedTask, state: State, plan: Plan): Record<string, TeamRoleAuthority> | undefined {
  const owners = root.metadata.ownerReferences ?? [];
  const teamOwned = owners.length === 1 && owners[0].kind === "KarsTeam"
    && owners[0].apiVersion === "kars.azure.com/v1alpha1" && owners[0].controller === true && !!owners[0].uid;
  const raw = root.metadata.annotations?.[teamRolesKey];
  requireFact(state.teamRoles === raw, "sealed Team roster binding changed");
  if (raw === undefined) {
    requireFact(!teamOwned, "reviewed Team run lacks sealed roster authority");
    return undefined;
  }
  requireFact(teamOwned && root.metadata.annotations?.[`${prefix}team-request-state`] === "admitted"
    && root.metadata.annotations?.[runKey] === root.metadata.name, "sealed roster lacks admitted Team custody");
  const roles = boundedJson(raw,64*1024) as Record<string, TeamRoleAuthority>;
  shape(roles,plan.roles.map(role => role.name));
  requireFact(Object.keys(roles).length === plan.roles.length, "sealed roster role set changed");
  for (const role of plan.roles) {
    const authority = roles[role.name];
    shape(authority,["blueprint","envelope"]);
    requireFact(authority.envelope && typeof authority.envelope === "object"
      && !Array.isArray(authority.envelope) && Number.isSafeInteger(authority.envelope.delegationDepth)
      && authority.envelope.delegationDepth! >= 0, "invalid sealed role envelope");
    requireFact(authority.blueprint === null || (authority.blueprint && typeof authority.blueprint === "object"
      && !Array.isArray(authority.blueprint)), "invalid sealed role blueprint");
  }
  return roles;
}

/** Reconstruct permission from the current reviewed root and exact child reservations.
 * Immutable artifact custody proves bytes, not permission; it is checked only after this graph fence.
 */
export async function reviewedMission(api: KubernetesJson, task: ReviewedTask, supplied: unknown,
  objective: (task: ReviewedTask, nonce: string) => string | null,
  readInputs: (references: unknown) => Promise<readonly MissionInput[]>): Promise<{
    contract: MissionContract; inputArtifacts?: readonly MissionInput[];
  } | null> {
  if (!needsReviewedAuthority(task)) { requireFact(supplied === undefined, "ordinary Task has a reviewed grant"); return null; }
  const g = supplied as { contract?: unknown; inputs?: unknown; authority?: Record<string,unknown> };
  requireFact(g && g.authority && Array.isArray(g.inputs), "missing installed reviewed grant");
  shape(g,["contract","inputs","authority"]);
  const authority = g.authority!;
  shape(authority,["rootName","rootUid","stateName","stateUid","slot"]);
  const ns = task.metadata.namespace!;
  const rootName = task.metadata.annotations?.[rootKey] ?? task.metadata.name;
  const root = rootName === task.metadata.name ? task : await api.request<ReviewedTask>("GET",taskPath(ns,rootName));
  const reads: Array<[string, unknown]> = [[taskPath(ns,root.metadata.name),root], [taskPath(ns,task.metadata.name),task]];
  currentReady(task); currentReady(root);
  requireFact(root.metadata.namespace === ns && root.metadata.name === rootName && root.spec.execution?.launch === true, "root is not current and Ready");
  requireFact(missionRuntime(root.spec) !== null
    && root.spec.envelope?.budget?.scope === "GovernedInference", "unsupported runtime or budget lineage");
  const pin = boundedJson(root.metadata.annotations?.[pinKey],1024) as {name:string; uid:string};
  shape(pin,["name","uid"]);
  requireFact(pin?.name && pin.uid && authority.rootName === rootName && authority.rootUid === root.metadata.uid
    && authority.stateName === pin.name && authority.stateUid === pin.uid, "root/state pin mismatch");
  const map = await api.request<Map>("GET",mapPath(ns,pin.name)); reads.push([mapPath(ns,pin.name),map]); owned(map,root);
  requireFact(map.metadata.uid === pin.uid, "reserved state replaced");
  const state = boundedJson(map.data?.["state.json"],900*1024) as State;
  shape(state,["schema","rootName","rootUid","admission","planDigest","plan","teamRoles","teamMemory","teamRevision","children"]);
  const plan = normalizePlan(root.spec.blueprint?.executionPlan);
  requireFact(Number.isSafeInteger(root.spec.envelope?.delegationDepth)
    && root.spec.envelope!.delegationDepth! >= (plan.roles.length === 0 ? 0 : 2), "unsupported delegation depth");
  const teamRoles = teamRoleAuthorities(root,state,plan);
  const memoryRaw = root.metadata.annotations?.[teamMemoryKey];
  requireFact(state.teamMemory === memoryRaw, "sealed Team memory binding changed");
  const memory = memoryRaw === undefined ? [] : missionInputs(boundedJson(memoryRaw,MAX_MISSION_RETAINED_INPUT_BYTES));
  requireFact(memoryRaw === undefined || (teamRoles !== undefined && memory.length <= 8
    && memory.every(input => input.name.startsWith("memory-") && input.source.namespace === ns)),
    "retained memory lacks admitted Team custody");
  const revisionRaw = root.metadata.annotations?.[teamRevisionKey];
  requireFact(state.teamRevision === revisionRaw, "sealed Team revision binding changed");
  const revisionValue = revisionRaw === undefined ? undefined : boundedJson(revisionRaw,MAX_MISSION_RETAINED_INPUT_BYTES);
  let revision: readonly MissionInput[] = [];
  let referencedRevision: readonly MissionInputReference[] = [];
  let diagnosticRoles: Array<{name: string; phases: string[]}> = [];
  if (revisionValue !== undefined) {
    requireFact(teamRoles !== undefined, "retained revision lacks admitted Team custody");
    let ordered: MissionInputReference[];
    if (Array.isArray(revisionValue)) {
      requireFact(revisionValue.length <= 16, "excessive inline revision documents");
      revision = missionInputs(revisionValue);
      ordered = revisionValue.map(({content: _content,...reference}) => reference);
    } else {
      shape(revisionValue,["schema","inputs","diagnosticRoles"]);
      const manifest = revisionValue as {schema: string; inputs: MissionInputReference[]; diagnosticRoles?: typeof diagnosticRoles};
      requireFact(manifest.schema === "kars.team-revision-inputs/v1", "unknown revision manifest schema");
      referencedRevision = missionInputReferences(manifest.inputs);
      ordered = manifest.inputs;
      const roles = manifest.diagnosticRoles === undefined ? [] : manifest.diagnosticRoles;
      requireFact(Array.isArray(roles) && roles.length <= 8, "excessive diagnostic role routing");
      const validName = (value: unknown): value is string => typeof value === "string" && value.length > 0 && Buffer.byteLength(value) <= 128;
      for (const role of roles) {
        shape(role,["name","phases"]);
        requireFact(validName(role.name) && Array.isArray(role.phases) && role.phases.length > 0 && role.phases.length <= 8
          && role.phases.every(validName) && new Set(role.phases).size === role.phases.length, "invalid diagnostic role routing");
      }
      requireFact(new Set(roles.map(role => role.name)).size === roles.length, "duplicate diagnostic role routing");
      diagnosticRoles = roles;
    }
    const producers = new Map<string,unknown>();
    let diagnosticsStarted = false;
    ordered.forEach((input,index) => {
      let group = "delivery";
      if (input.name.startsWith("revision-unaccepted-")) {
        diagnosticsStarted = true;
        const alias = /^revision-unaccepted-(r(0|[1-9][0-9]*)-p(0|[1-9][0-9]*))(?:\.md|--[a-zA-Z0-9._-]+)$/.exec(input.name);
        requireFact(alias !== null, "invalid diagnostic revision alias");
        group = alias![1];
        if (diagnosticRoles.length) {
          const r = Number(alias![2]), p = Number(alias![3]);
          requireFact(Number.isSafeInteger(r) && Number.isSafeInteger(p)
            && diagnosticRoles[r]?.phases.length === p + 1, "diagnostic routing is not a retained final phase");
        }
      } else {
        requireFact(!diagnosticsStarted && input.name === `revision-${index}`, "invalid rejected-delivery revision alias");
      }
      const {artifactName,...source} = input.source;
      if (!producers.has(group)) producers.set(group,source);
      requireFact(source.namespace === ns && same(source,producers.get(group))
        && !["response.md","kars-router-observations.json"].includes(artifactName),
        "retained revision lacks matching producer provenance");
    });
  }
  const base = [...memory,...revision];
  const cap = root.spec.envelope?.budget?.tokens;
  requireFact(cap === undefined || (Number.isSafeInteger(cap) && cap >= 0), "invalid root token cap");
  requireFact(!cap || plan.roles.every(role => role.budgetTokens === undefined || role.budgetTokens <= cap), "role budget exceeds root cap");
  shape(state.children,plan.roles.flatMap((role,r) => [slot(r),...role.phases.map((_,p) => slot(r,p))]));
  const nonce = root.metadata.annotations?.[runKey] ?? "";
  requireFact(typeof nonce === "string" && nonce.length > 0, "missing root run");
  const content = objective(root,nonce);
  const admission = {version:1,state:"run",taskGeneration:root.metadata.generation,authorizationDigest:root.status?.envelopeDigest,runNonce:nonce,objectiveDigest:content && hash(content)};
  const expectedStateName = `kars-reviewed-${hash(JSON.stringify([root.metadata.uid,admission.taskGeneration,admission.authorizationDigest,nonce,admission.objectiveDigest])).slice(7,47)}`;
  requireFact(content && ["kars.reviewed-run/v1", "kars.reviewed-run/v2"].includes(state?.schema) && state.rootName === rootName && state.rootUid === root.metadata.uid
    && same(state.admission,admission) && same(state.plan,plan) && state.planDigest === hash(JSON.stringify(sorted(plan)))
    && pin.name === expectedStateName, "review/run/plan binding changed");
  const checkChild = async (r: number,p?: number): Promise<ReviewedTask> => {
    const key = slot(r,p); const reserved = state.children[key];
    requireFact(reserved?.uid && reserved.name, "unreserved input or phase");
    const parent = p === undefined ? root : await checkChild(r);
    shape(reserved,["name","uid","spec"]);
    const child = await api.request<ReviewedTask>("GET",taskPath(ns,reserved.name)); reads.push([taskPath(ns,reserved.name),child]); owned(child,parent); currentReady(child);
    requireFact(child.metadata.namespace === ns && child.metadata.name === reserved.name, "child path changed");
    const expected = structuredClone(parent.spec);
    expected.parentRef = {name:parent.metadata.name};
    expected.envelope!.tier = parent.spec.envelope!.authorityCeiling;
    expected.envelope!.delegationDepth = parent.spec.envelope!.delegationDepth! - 1;
    if (p === undefined && teamRoles !== undefined) {
      const authority = teamRoles[plan.roles[r].name];
      const depth = expected.envelope!.delegationDepth!;
      expected.envelope = structuredClone(authority.envelope);
      expected.envelope.delegationDepth = Math.min(expected.envelope.delegationDepth!,depth);
      expected.blueprint = structuredClone(authority.blueprint ?? {});
    }
    if (p === undefined && plan.roles[r].budgetTokens !== undefined) {
      const existing = expected.envelope!.budget?.tokens;
      const limit = plan.roles[r].budgetTokens!;
      expected.envelope!.budget ??= {};
      expected.envelope!.budget.tokens = existing !== undefined && existing > 0 ? Math.min(existing,limit) : limit;
    }
    delete expected.blueprint!.executionPlan;
    for (const field of ["modelFallbacks","mcpServers","egress"]) {
      if (Array.isArray(expected.blueprint![field]) && !expected.blueprint![field].length) delete expected.blueprint![field];
    }
    const phaseObjective = p === undefined ? undefined : `${content}\n\nRole: ${plan.roles[r].objective}\n\nPhase: ${plan.roles[r].phases[p].objective}`;
    const boundObjective = phaseObjective !== undefined && Array.from(phaseObjective).length > 4096 ? phaseObjective : undefined;
    expected.objective = boundObjective === undefined
      ? phaseObjective ?? plan.roles[r].objective
      : `Role: ${plan.roles[r].objective}\n\nPhase: ${plan.roles[r].phases[p!].objective}`;
    const childRun = `plan-${pin.name.slice(14)}-${key}`;
    const objectiveAnnotations = {
      "run-objective-nonce": boundObjective === undefined ? undefined : childRun,
      "run-objective-b64": boundObjective === undefined ? undefined : Buffer.from(boundObjective).toString("base64"),
      "run-objective-digest": boundObjective === undefined ? undefined : hash(boundObjective),
    };
    requireFact(Object.entries(objectiveAnnotations).every(([name,value]) => child.metadata.annotations?.[`${prefix}${name}`] === value),
      "reserved child objective binding changed");
    expected.displayName = p === undefined ? plan.roles[r].name : `${plan.roles[r].name} / ${plan.roles[r].phases[p].name}`;
    expected.execution = {launch:p !== undefined,
      ...(!(p === undefined && teamRoles !== undefined) && parent.spec.execution?.runtime !== undefined ? {runtime:parent.spec.execution.runtime} : {})};
    requireFact(missionRuntime(expected) !== null, "unsupported reviewed role runtime");
    requireFact(child.metadata.uid === reserved.uid && reserved.name === `rp-${pin.name.slice(14,34)}-${key}`
      && same(reserved.spec,expected) && same(child.spec,expected) && child.metadata.annotations?.[rootKey] === rootName
      && same(boundedJson(child.metadata.annotations?.[pinKey],1024),pin) && child.metadata.annotations?.[slotKey] === key
      && child.metadata.annotations?.[runKey] === childRun, "reserved child authority changed");
    return child;
  };
  let phase: FilesystemPhase; let required: string[]; let sources: Array<[number,number]>;
  if (task.metadata.name === rootName) {
    requireFact(authority.slot === "synthesis", "root may only synthesize");
    phase = filesystemPhase({name:"synthesis",...plan.synthesis,minToolCalls:0,freshContext:true,requiredToolCalls:[]});
    required = plan.deliverables!.map(d => d.name);
    sources = plan.roles.map((r,i) => [i,r.phases.length-1]);
  } else {
    const key = task.metadata.annotations?.[slotKey];
    let coordinates: [number,number] | undefined;
    plan.roles.forEach((r,i) => r.phases.forEach((_,p) => {if (slot(i,p) === key) coordinates = [i,p];}));
    requireFact(coordinates && authority.slot === key, "unknown phase slot");
    const [r,p] = coordinates!;
    const actual = await checkChild(r,p);
    if (!same(actual,task)) throw new ReviewedSnapshotChangedError("Reviewed execution: phase changed during admission");
    phase = filesystemPhase(plan.roles[r].phases[p]); required = ["handoff.md"];
    sources = p > 0 ? [[r,p-1]] : plan.roles[r].dependsOn!.map(name => {
      const i = plan.roles.findIndex(r => r.name === name); return [i,plan.roles[i].phases.length-1];
    });
  }
  const refs = (g.inputs as unknown[]).length ? missionInputReferences(g.inputs) : [];
  const baseInput = (input: MissionInputReference): boolean => input.name.startsWith("memory-") || input.name.startsWith("revision-");
  const peerRefs = refs.filter(input => !baseInput(input));
  const selectedRevision = referencedRevision.filter(historical => {
    const coordinates = /^revision-unaccepted-r([0-9]+)-p([0-9]+)(?:\.md|--)/.exec(historical.name);
    if (!coordinates || !diagnosticRoles.length) return true;
    const role = diagnosticRoles[Number(coordinates[1])];
    const phase = role.phases[Number(coordinates[2])];
    return !peerRefs.some(peer => {
      const current = /^r([0-9]+)-p([0-9]+)(?:\.md|--)/.exec(peer.name);
      if (!current) return false;
      const currentRole = plan.roles[Number(current[1])];
      return currentRole?.name === role.name && currentRole.phases[Number(current[2])]?.name === phase
        && peer.source.artifactName === historical.source.artifactName;
    });
  });
  const baseRefs = base.length + selectedRevision.length
    ? missionInputReferences([...base.map(({content: _content,...reference}) => reference),...selectedRevision]) : [];
  requireFact(refs.length >= sources.length + baseRefs.length
    && same(refs.filter(baseInput),baseRefs),
    "dependency, memory or revision input set differs from the sealed run");
  const selected = new Set<string>();
  for (const [r,p] of sources) {
    const child = await checkChild(r,p);
    const ref = peerRefs.find(i => i.name === `r${r}-p${p}.md`) as MissionInputReference | undefined;
    requireFact(ref && ref.source.namespace === ns && ref.source.taskName === child.metadata.name && ref.source.taskUid === child.metadata.uid
      && ref.source.runNonce === child.metadata.annotations?.[runKey] && ref.source.artifactName === "handoff.md"
      && child.metadata.annotations?.[`${prefix}run-completed`] === ref.source.runNonce, "input is not the reviewed successful dependency");
    const documents = peerRefs.filter(input => input.source.taskUid === child.metadata.uid)
      .sort((a,b) => a.source.artifactName < b.source.artifactName ? -1 : a.source.artifactName > b.source.artifactName ? 1 : 0);
    requireFact(documents.length <= 16
      && new Set(documents.map(input => input.source.artifactName)).size === documents.length,
      "duplicate or excessive dependency documents");
    for (const [index, document] of documents.entries()) {
      const {artifactName, ...source} = document.source;
      const {artifactName: _handoff, ...expectedSource} = ref!.source;
      const alias = artifactName === "handoff.md" ? `r${r}-p${p}.md` : `r${r}-p${p}--${index}-${artifactName}`.slice(0,128);
      requireFact(same(source,expectedSource) && document.name === alias
        && !["response.md","kars-router-observations.json"].includes(artifactName),
        "dependency document source or alias differs from the reviewed handoff");
      selected.add(document.name);
    }
  }
  requireFact(selected.size === peerRefs.length, "input is outside the reviewed dependencies");
  // Validate the complete retained history even when current dependency exports
  // supersede mounted diagnostics. Each read remains within the existing bound.
  const history = referencedRevision.length ? missionInputs(await readInputs(referencedRevision)) : [];
  requireFact(same(history.map(({content: _content,...reference}) => reference),referencedRevision),
    "complete historical custody differs from the sealed revision");
  const peers = peerRefs.length ? await readInputs(peerRefs) : [];
  const selectedNames = new Set(selectedRevision.map(input => input.name));
  const retained = refs.length ? missionInputs([...base,...history.filter(input => selectedNames.has(input.name)),...peers]) : undefined;
  const peerContext = state.schema === "kars.reviewed-run/v2" ? {
    rootUid: state.rootUid, stateName: pin.name, slot: authority.slot,
    peers: plan.roles.flatMap((role, r) => role.phases.map((phase, p) => ({
      role: role.name, phase: phase.name, slot: slot(r, p), agentName: `rp-${pin.name.slice(14, 34)}-${slot(r, p)}`,
    }))),
  } : undefined;
  const contract = missionContract(phase,retained,required,peerContext);
  const installed = parseMissionContract(g.contract);
  requireFact(installed && sameMissionContract(installed,contract), "installed phase/input/output requirements differ");
  for (const [path,prior] of reads.reverse()) {
    if (!same(await api.request("GET",path),prior)) {
      throw new ReviewedSnapshotChangedError(`Reviewed execution: authority changed during custody traversal: ${path}`);
    }
  }
  return {contract,...(retained ? {inputArtifacts:retained} : {})};
}
