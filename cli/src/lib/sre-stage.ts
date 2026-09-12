// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { parseAllDocuments } from "yaml";
import { get, requireRegistrar, type ApiObject, type Execute } from "./sre-authority.js";
import { listSreHelmReleases, sreHelmStageWait } from "./sre-helm.js";
import { ACTION_CRD, planActionCrd } from "./sre-action-crd.js";
import { prepareCoreHelmSchemas } from "./core-helm-schemas.js";
import { waitForInstalledCoreSchemas } from "./schema-stage.js";

type StagePhase = "registrar" | "controller-review" | "release-inventory" | "prerequisite-chart-render"
  | "action-schema-review" | "helm-compatibility" | "action-schema-migration" | "core-schema-preparation"
  | "helm-server-dry-run" | "helm-upgrade" | "template-ownership-review" | "schema-publication"
  | "template-authority-write" | "controller-rollout";

function parts(image: string): [string,string] {
  const index=image.lastIndexOf(":");
  if(index<=image.lastIndexOf("/")||image.includes("@"))throw new Error("Stage images require explicit repository:tag references");
  return [image.slice(0,index),image.slice(index+1)];
}

function canonical(value:any):string {
  const sort=(value:any):any=>Array.isArray(value)?value.map(sort)
    :value&&typeof value==="object"?Object.fromEntries(Object.keys(value).sort().map(key=>[key,sort(value[key])])):value;
  return JSON.stringify(sort(value));
}

export async function stageAuthority(
  execute:Execute,chart:string,namespace:string,release:string,
  controllerImage:string,routerImage:string,dryRun:boolean,
):Promise<void> {
  let phase: StagePhase = "registrar";
  try {
    await stageAuthorityChecked(execute,chart,namespace,release,controllerImage,routerImage,dryRun,
      next => { phase = next; });
  } catch (error) {
    // Fixed source stage only: never include argv, API bodies or raw causes.
    console.error(`SRE-STAGE-FAILURE ${phase}`);
    throw error;
  }
}

async function stageAuthorityChecked(
  execute:Execute,chart:string,namespace:string,release:string,
  controllerImage:string,routerImage:string,dryRun:boolean,mark:(phase:StagePhase)=>void,
):Promise<void> {
  await requireRegistrar(execute);
  mark("controller-review");
  const controller=await get(execute,"deployment","kars-controller",namespace);
  if(!controller)throw new Error("Install the core prerequisite first; authority staging does not provision a new cluster");
  if(controller.spec?.template?.spec?.serviceAccountName!=="kars-controller") {
    throw new Error("Controller uses a custom ServiceAccount; review and stage its minimal authority role explicitly");
  }
  mark("release-inventory");
  const stdout=await listSreHelmReleases(execute,namespace);
  const releases=JSON.parse(stdout) as unknown;
  if(!Array.isArray(releases)||releases.some(item=>!item||typeof item.name!=="string"||item.namespace!==namespace)) {
    throw new Error("Helm ownership inventory is invalid");
  }
  const [controllerRepository,controllerTag]=parts(controllerImage);
  const [routerRepository,routerTag]=parts(routerImage);
  const helm=releases.some(item=>item.name===release);
  mark("prerequisite-chart-render");
  const rendered=await execute("helm",["template",release,chart,"--namespace",namespace,
    "--set","sre.enabled=false","--set","azure.workloadIdentity.clientId=dummy"],{stdio:"pipe"});
  const documents=parseAllDocuments(rendered.stdout).map(doc=>{
    if(doc.errors.length)throw new Error(`Invalid staged chart YAML: ${doc.errors[0].message}`);
    return doc.toJSON() as ApiObject|null;
  }).filter((obj):obj is ApiObject=>!!obj);
  const actions=documents.filter(obj=>obj.kind==="CustomResourceDefinition"&&obj.metadata.name===ACTION_CRD);
  if(actions.length!==1)throw new Error("Exactly one compatible action CRD is required before staging authority policies");
  mark("action-schema-review");
  const stageAction=await planActionCrd(execute,actions[0],namespace,release,helm);
  if(helm) {
    mark("helm-compatibility");
    const wait=await sreHelmStageWait(execute);
    const args=["upgrade",release,chart,"--namespace",namespace,"--reset-then-reuse-values",
      "--set","sre.authorityStage=true",
      "--set-string",`controller.image.repository=${controllerRepository}`,
      "--set-string",`controller.image.tag=${controllerTag}`,
      "--set-string",`inferenceRouter.image.repository=${routerRepository}`,
      "--set-string",`inferenceRouter.image.tag=${routerTag}`,
      ...(dryRun?["--dry-run=server"]:[wait,"--timeout","8m"])];
    if(!dryRun) {
      // This explicit authority-stage command retains its existing complete-
      // fingerprint migration, not a generic ownership-digest exception.
      // It is non-atomic; ordinary/atomic upgrades cannot invoke this repair.
      mark("action-schema-migration");
      await stageAction();
      mark("core-schema-preparation");
      await prepareCoreHelmSchemas(execute,args);
    }
    mark(dryRun?"helm-server-dry-run":"helm-upgrade");
    await execute("helm",args,{stdio:"pipe"});
    return;
  }
  mark("template-ownership-review");
  if(controller.metadata.annotations?.["meta.helm.sh/release-name"]) {
    throw new Error("Controller reports Helm ownership that was not found; no template-mode adoption is allowed");
  }
  const allowedRoles=["kars-sre-registrar","kars-sre-router-renew","kars-sre-private-diagnostics","kars-sre-retired-agent","kars-sre-authority-controller"];
  const objects=documents
    .filter(obj=>(obj.kind==="CustomResourceDefinition"&&obj.metadata.name==="karssreregistrations.kars.azure.com")
      || (["ValidatingAdmissionPolicy","ValidatingAdmissionPolicyBinding"].includes(obj.kind!)&&obj.metadata.name?.startsWith("kars-sre-"))
      || (obj.kind==="ClusterRole"&&allowedRoles.includes(obj.metadata.name!))
      || (obj.kind==="ClusterRoleBinding"&&obj.metadata.name==="kars-sre-authority-controller"));
  if(!objects.some(obj=>obj.kind==="CustomResourceDefinition"))throw new Error("Authority CRD is absent from the staged chart");
  const writes:Array<{object:ApiObject;existing?:ApiObject}>=[];
  const unchangedCrds:string[]=[];
  for(const object of objects) {
    const existing=await get(execute,object.kind!.toLowerCase(),object.metadata.name!);
    if(existing) {
      const desired=object.spec??object.rules??{roleRef:object.roleRef,subjects:object.subjects};
      const current=existing.spec??existing.rules??{roleRef:existing.roleRef,subjects:existing.subjects};
      if(canonical(desired)===canonical(current)) {
        if(object.kind==="CustomResourceDefinition")unchangedCrds.push(object.metadata.name!);
        continue;
      }
      if(existing.metadata.annotations?.["kars.azure.com/sre-authority-staged"]!==namespace
          || existing.metadata.annotations?.["kars.azure.com/sre-authority-release"]!==release) {
        throw new Error(`Unowned authority object ${object.kind}/${object.metadata.name} differs; no objects were adopted`);
      }
    }
    writes.push({object,existing});
  }
  const containers=structuredClone(controller.spec?.template?.spec?.containers);
  if(!Array.isArray(containers))throw new Error("Controller container specification is missing");
  const main=containers.find(container=>container.name==="controller");
  if(!main)throw new Error("Controller container identity is unrecognized");
  main.image=controllerImage;
  main.env=(main.env??[]).filter((entry:{name:string})=>entry.name!=="INFERENCE_ROUTER_IMAGE");
  main.env.push({name:"INFERENCE_ROUTER_IMAGE",value:routerImage});
  if(dryRun) {
    console.log(`Would verify/CAS-repair the action API prerequisite, stage ${writes.length} authority objects and CAS-update controller ${controller.metadata.uid}@${controller.metadata.resourceVersion}`);
    return;
  }
  mark("action-schema-migration");
  await stageAction();
  for(const name of unchangedCrds) {
    await execute("kubectl",["wait","--for=condition=Established",`crd/${name}`,"--timeout=60s"],{stdio:"pipe"});
  }
  let schemasPublished=false;
  for(const {object,existing} of writes.sort((a,b)=>Number(b.object.kind==="CustomResourceDefinition")-Number(a.object.kind==="CustomResourceDefinition"))) {
    if(object.kind!=="CustomResourceDefinition"&&!schemasPublished) {
      mark("schema-publication");
      await waitForInstalledCoreSchemas(execute,documents);
      schemasPublished=true;
    }
    const annotations={...object.metadata.annotations,
      "kars.azure.com/sre-authority-staged":namespace,"kars.azure.com/sre-authority-release":release};
    mark("template-authority-write");
    if(existing) {
      await execute("kubectl",["patch",object.kind!.toLowerCase(),object.metadata.name!,"--type=merge","-p",JSON.stringify({
        ...object,metadata:{uid:existing.metadata.uid,resourceVersion:existing.metadata.resourceVersion,annotations},
      })],{stdio:"pipe"});
    } else {
      await execute("kubectl",["create","-f","-"],{stdio:"pipe",input:JSON.stringify({
        ...object,metadata:{...object.metadata,annotations},
      })});
    }
    if(object.kind==="CustomResourceDefinition") {
      await execute("kubectl",["wait","--for=condition=Established",`crd/${object.metadata.name}`,"--timeout=60s"],{stdio:"pipe"});
    }
  }
  if(!schemasPublished) {
    mark("schema-publication");
    await waitForInstalledCoreSchemas(execute,documents);
  }
  mark("controller-rollout");
  await execute("kubectl",["patch","deployment","kars-controller","-n",namespace,"--type=merge","-p",JSON.stringify({
    metadata:{uid:controller.metadata.uid,resourceVersion:controller.metadata.resourceVersion},
    spec:{template:{spec:{containers}}},
  })],{stdio:"pipe"});
  await execute("kubectl",["rollout","status","deployment/kars-controller","-n",namespace,"--timeout=8m"],{stdio:"pipe"});
}
