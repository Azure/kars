// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { execa } from "execa";
import {
  previewPrivateActivation, stagePrivateActivation, validatePrivateActivation,
  validateQualifiedActivation, canonical, type PrivateActivation,
  verifyOwnedRuntimeNamespace,
} from "../lib/private-activation.js";

type Execute=(args:string[],input?:string)=>Promise<string>;
const resource="karscredentialgrants.kars.azure.com";
const standard=["TELEGRAM_BOT_TOKEN","TELEGRAM_ALLOW_FROM","SLACK_BOT_TOKEN","DISCORD_BOT_TOKEN","WHATSAPP_ENABLED",
  "BRAVE_API_KEY","TAVILY_API_KEY","EXA_API_KEY","FIRECRAWL_API_KEY","PERPLEXITY_API_KEY"];

export function agentCredentialKey(key:string):boolean {
  return /^[A-Z_][A-Z0-9_]{0,127}$/.test(key)
    && !/^(AGT_|AZURE_|IMDS_|KARS_|FOUNDRY_|KUBERNETES_|LD_|DYLD_|NODE_|PYTHON|BASH|ENV_|SSL_|RUST_|CARGO_|GIT_|SSH_|OPENAI_|ANTHROPIC_|GEMINI_|GOOGLE_|OLLAMA_|COPILOT_)/.test(key)
    && !["HTTP_PROXY","HTTPS_PROXY","ALL_PROXY","NO_PROXY","AWS_ACCESS_KEY_ID","AWS_SECRET_ACCESS_KEY","AWS_SESSION_TOKEN"].includes(key)
    && (standard.includes(key)||/(_TOKEN|_KEY|_SECRET|_PASSWORD|_PAT|_CREDENTIAL|_CREDENTIALS|_CONNECTION_STRING|_AUTH|_AUTHORIZATION)$/.test(key));
}

async function get(execute:Execute,kind:string,name:string,namespace?:string):Promise<any|undefined>{
  const text=await execute(["get",kind,name,...(namespace?["-n",namespace]:[]),"--ignore-not-found","-o","json"]);
  if(!text.trim())return undefined;
  const object=JSON.parse(text);
  if(object===null)return undefined;
  if(!object.metadata?.uid||!object.metadata.resourceVersion||object.metadata.deletionTimestamp)
    throw new Error("Credential preflight requires an exact live API UID/resourceVersion");
  return object;
}

function storeKey(purpose:string,name:string,key:string):boolean {
  switch(purpose){
    case "providers":return name==="kars-inference-providers"&&(key==="COPILOT_GITHUB_TOKEN"||/^KARS_PROVIDER_[A-Z0-9_]+_(ENDPOINT|API_KEY|TOKEN|MODELS)$/.test(key));
    case "foundry":return name==="kars-foundry-credentials"&&key==="FOUNDRY_API_KEY";
    case "provider-default":return name.startsWith("kars-provider-")&&key==="API_KEY";
    case "github-app":return name==="kars-github-app"&&["GITHUB_APP_ID","GITHUB_APP_PRIVATE_KEY"].includes(key);
    case "github-connection":return name==="kars-github-connection"&&["GITHUB_TOKEN","GITHUB_OWNER","GITHUB_REPO"].includes(key);
    case "teams":return ["client-id","tenant-id","client-secret","entra-role-map","bff-internal-secret"].includes(key);
    case "controller-settings":return name==="kars-credential-controller-settings"&&key==="configuration";
    default:return false;
  }
}

export async function validateGrantDocument(execute:Execute,document:any):Promise<void>{
  if(document.apiVersion!=="kars.azure.com/v1alpha1"||document.kind!=="KarsCredentialGrant"
    ||document.metadata?.name!=="workspace"||!document.metadata.namespace||!document.spec
    ||Object.keys(document.spec).some(key=>!["workspaceUid","writers","privateActivation","agentKeys","integrationStores","legacyImports","controller","bridgeConsumers","observationTargets","githubConnections","enabled"].includes(key)))
    throw new Error("Only a metadata-only workspace credential grant is accepted");
  const ns=document.metadata.namespace;
  if((await execute(["auth","can-i","manage",`${resource}/workspace`,"-n",ns])).trim()!=="yes")
    throw new Error("Explicit credential-grant operator permission is required");
  if((await get(execute,"namespace",ns))?.metadata.uid!==document.spec.workspaceUid)
    throw new Error("Reviewed workspace UID changed");
  if(!Array.isArray(document.spec.writers)||document.spec.writers.length>16)throw new Error("A reviewed writer list (at most 16 identities) is required");
  for(const writer of document.spec.writers){
    if((await get(execute,"serviceaccount",writer.name,writer.namespace))?.metadata.uid!==writer.uid)
      throw new Error("Reviewed writer ServiceAccount UID changed");
  }
  for(const key of document.spec.agentKeys??[])if(!agentCredentialKey(key))
    throw new Error(`Agent key ${key} is reserved or is not a credential key`);
  for(const store of document.spec.integrationStores??[]){
    const actual=await get(execute,"secret",store.secret.name,ns);
    if(actual?.metadata.uid!==store.secret.uid||actual.type!=="Opaque")
      throw new Error("Reviewed integration store UID/type changed");
    if(Object.keys(actual.data??{}).some(key=>!storeKey(store.purpose,store.secret.name,key)))
      throw new Error("Existing integration keys do not match the reviewed purpose; nothing was mutated");
  }
  for(const target of document.spec.observationTargets??[]){
    if(target.kind!=="KarsSandbox"||target.namespace!==ns
      ||(await get(execute,"karssandbox",target.name,ns))?.metadata.uid!==target.uid)
      throw new Error("Reviewed observation Sandbox UID changed");
  }
  if((document.spec.githubConnections??[]).length>32)throw new Error("At most 32 GitHub connections may be enrolled");
  for(const approved of document.spec.githubConnections??[]){
    if(Object.keys(approved).some(key=>!["connection","appSecret","appId","ownerSubject","installationId","repositories","write"].includes(key))
      ||typeof approved.ownerSubject!=="string"||!approved.ownerSubject
      ||!Number.isSafeInteger(approved.installationId)||approved.installationId<=0
      ||typeof approved.appId!=="string"||!/^[0-9]{1,20}$/.test(approved.appId)||BigInt(approved.appId)===0n
      ||!Array.isArray(approved.repositories)||!approved.repositories.length||approved.repositories.length>32
      ||approved.repositories.some((repo:unknown)=>typeof repo!=="string"||!/^[a-z0-9._-]{1,39}\/[a-z0-9._-]{1,100}$/.test(repo)
        ||repo.split("/").some(part=>[".",".."].includes(part))))
      throw new Error("GitHub enrollment must contain only canonical reviewed metadata");
    const expected=`kars-github-connection-${createHash("sha256").update(approved.ownerSubject).digest("hex").slice(0,16)}`;
    const source=await get(execute,"configmap",approved.connection.name,ns);
    const store=await get(execute,"secret",approved.appSecret.name,ns);
    const repos=JSON.parse(source?.data?.repos??"[]");
    if(approved.connection.name!==expected||source?.metadata.uid!==approved.connection.uid
      ||store?.metadata.uid!==approved.appSecret.uid||store?.type!=="Opaque"
      ||Buffer.from(store?.data?.GITHUB_APP_ID??"","base64").toString("utf8")!==approved.appId
      ||String(approved.installationId)!==source?.data?.installation_id
      ||!document.spec.integrationStores?.some((entry:any)=>entry.purpose==="github-app"
        &&entry.secret.name===approved.appSecret.name&&entry.secret.uid===approved.appSecret.uid)
      ||!Array.isArray(repos)||approved.repositories.some((repo:string)=>!repos.some((value:unknown)=>typeof value==="string"&&value.toLowerCase()===repo)))
      throw new Error("GitHub App/connection UID, installation or repository review changed; nothing was mutated");
  }
  const deployments=[document.spec.controller,document.spec.bridgeConsumers?.bff,document.spec.bridgeConsumers?.gateway].filter(Boolean);
  for(const deployment of deployments)if((await get(execute,"deployment",deployment.name,ns))?.metadata.uid!==deployment.uid)
    throw new Error("Reviewed integration Deployment UID changed");
  for(const review of document.spec.legacyImports??[]){
    const actual=await get(execute,"secret",review.secret.name,review.namespace);
    const namespace=await get(execute,"namespace",review.namespace);
    if(actual?.metadata.uid!==review.secret.uid||actual.metadata.resourceVersion!==review.resourceVersion
      ||namespace?.metadata.uid!==review.namespaceUid||actual.type!=="Opaque"
      ||JSON.stringify(Object.keys(actual.data??{}).sort())!==JSON.stringify([...review.keys].sort()))
      throw new Error("Legacy credential UID/resourceVersion/key-name review changed; nothing was mutated");
    for(const key of review.keys)if(!(key==="TEAMS_ENABLED"&&!review.target)&&!standard.includes(key)&&!document.spec.agentKeys?.includes(key))
      throw new Error(`Legacy key ${key} is not granted; existing values are preserved`);
  }
  if(document.spec.enabled!==false&&document.spec.writers.length){
    await validatePrivateActivation(execute,document.spec.privateActivation as PrivateActivation);
    const activation=document.spec.privateActivation as PrivateActivation;
    const required=[...new Set([ns,activation.root.namespace.name,
      ...(activation.root.budgetTls?[activation.root.budgetTls.namespace.name]:[]),
      ...document.spec.writers.map((writer:any)=>writer.namespace),
      ...(document.spec.observationTargets??[]).map((target:any)=>`kars-${target.name}`)])].sort();
    const selected=activation.namespaces.map(scope=>scope.namespace.name);
    if(required.some(name=>!selected.includes(name)))
      throw new Error("Private activation must cover this grant's protected namespaces");
    for(const name of selected.filter(name=>!required.includes(name)))
      await verifyOwnedRuntimeNamespace(execute,ns,name);
  }
}

export async function applyReviewedGrant(run:Execute,document:any):Promise<void> {
  await validateGrantDocument(run,document);
  let existing=await get(run,resource,"workspace",document.metadata.namespace);
  if(existing&&(existing.metadata.uid!==document.metadata.uid||existing.metadata.resourceVersion!==document.metadata.resourceVersion))
    throw new Error("Grant changed since review; regenerate the metadata-only preview");
  if(!existing&&(document.metadata.uid||document.metadata.resourceVersion))throw new Error("Reviewed grant disappeared");
  let stagedSpec=structuredClone(document.spec);
  let quiescentSpec:unknown;
  if(document.spec.enabled!==false&&document.spec.writers.length){
    if(existing&&existing.spec.writers.length){
      quiescentSpec={...existing.spec,writers:[]};
      await run(["patch",resource,"workspace","-n",document.metadata.namespace,"--type=merge","-p",JSON.stringify({
        metadata:{uid:existing.metadata.uid,resourceVersion:existing.metadata.resourceVersion},spec:quiescentSpec,
      })]);
      const deadline=Date.now()+120_000;
      for(;;){
        const current=await get(run,resource,"workspace",document.metadata.namespace);
        if(!current||current.metadata.uid!==existing.metadata.uid||canonical(current.spec)!==canonical(quiescentSpec))
          throw new Error("Grant changed while retiring prior private writer authority");
        if(current.status?.observedGeneration===current.metadata.generation
          &&current.status?.conditions?.some((c:any)=>c.type==="WriterReady"&&c.status==="False")){
          const inventory=JSON.parse(await run(["get","roles,rolebindings,clusterroles,clusterrolebindings",
            "--all-namespaces","--chunk-size=0","-o","json"]));
          if(!Array.isArray(inventory.items)||inventory.metadata?.continue)
            throw new Error("Private authority retirement inventory is incomplete");
          if(!inventory.items.some((object:any)=>
            object.metadata?.annotations?.["kars.azure.com/credential-grant-owner"]===existing.metadata.uid)){
            existing=current;break;
          }
        }
        if(Date.now()>=deadline)throw new Error("Prior writer authority retirement is still pending; no new activation was published");
        await new Promise(resolve=>setTimeout(resolve,500));
      }
    }
    stagedSpec.privateActivation=await stagePrivateActivation(run,document.spec.privateActivation);
    await validateQualifiedActivation(run,stagedSpec.privateActivation);
  }
  if(existing){
    const current=await get(run,resource,"workspace",document.metadata.namespace);
    if(!current||current.metadata.uid!==existing.metadata.uid
      ||canonical(current.spec)!==canonical(quiescentSpec??existing.spec))
      throw new Error("Grant changed before qualified publication");
    await run(["patch",resource,"workspace","-n",document.metadata.namespace,"--type=merge","-p",JSON.stringify({
      metadata:{uid:current.metadata.uid,resourceVersion:current.metadata.resourceVersion},spec:stagedSpec,
    })]);
  }else{
    await run(["create","-f","-"],JSON.stringify({...document,spec:stagedSpec}));
  }
}

export function credentialGrantsCommand():Command {
  const command=new Command("grant").description("Preview and explicitly apply operator-owned credential authority");
  const execute=(context?:string):Execute=>async(args,input)=>{
    const result=await execa("kubectl",[...(context?["--context",context]:[]),...args],{stdio:"pipe",...(input?{input}:{})});
    return result.stdout;
  };
  const repeat=(value:string,prior:string[])=>[...prior,value];
  command.command("preview").requiredOption("--namespace <namespace>")
    .requiredOption("--writer <namespace/name>","Writer ServiceAccount",repeat,[])
    .option("--agent-key <key>","Explicit custom agent credential key",repeat,[])
    .option("--store <name=purpose>","Existing operator store",repeat,[])
    .option("--controller","Enroll this workspace's controller Deployment")
    .option("--bridge-consumers","Enroll the existing BFF and Teams gateway Deployments")
    .option("--observe <sandbox>","Explicit Sandbox target for private read-only observations",repeat,[])
    .option("--private-root <namespace>","Explicit installed controller namespace for private capability activation")
    .option("--private-controller-profile <profile>","service-accounts or kcm-certificate")
    .option("--private-consumer <namespace/Kind/name>","Explicit reviewed existing private consumer",repeat,[])
    .option("--github-review <file>","Reviewed metadata-only GitHub connection/App/repository enrollments")
    .option("--legacy-review <file>","Reviewed legacySources metadata from the grant status")
    .option("--context <context>")
    .action(async options=>{
      const run=execute(options.context);
      const namespace=await get(run,"namespace",options.namespace);
      if(!namespace)throw new Error("The workspace must already exist");
      const writers=[];
      for(const raw of options.writer){
        const [ns,name,...extra]=raw.split("/");
        if(!ns||!name||extra.length)throw new Error("--writer must be namespace/name");
        const sa=await get(run,"serviceaccount",name,ns);
        if(!sa)throw new Error("Install the private add-on ServiceAccount before enrollment");
        writers.push({namespace:ns,name,uid:sa.metadata.uid});
      }
      const stores=[];
      for(const raw of options.store){
        const [name,purpose,...extra]=raw.split("=");
        if(!name||!purpose||extra.length)throw new Error("--store must be name=purpose");
        const store=await get(run,"secret",name,options.namespace);
        if(!store)throw new Error(`Bootstrap the explicitly selected empty Opaque store ${name} before preview; no existing object is adopted`);
        stores.push({secret:{name,uid:store.metadata.uid},purpose});
      }
      const identity=async(name:string)=>{
        const object=await get(run,"deployment",name,options.namespace);
        if(!object)throw new Error(`Deployment ${name} is missing`);
        return {name,uid:object.metadata.uid};
      };
      const existing=await get(run,resource,"workspace",options.namespace);
      const document={apiVersion:"kars.azure.com/v1alpha1",kind:"KarsCredentialGrant",
        metadata:{name:"workspace",namespace:options.namespace,...(existing?{uid:existing.metadata.uid,resourceVersion:existing.metadata.resourceVersion}:{})},
        spec:{workspaceUid:namespace.metadata.uid,writers,agentKeys:options.agentKey,integrationStores:stores,
          legacyImports:options.legacyReview?JSON.parse(readFileSync(options.legacyReview,"utf8")):[],
          enabled:true,observationTargets:[],
          githubConnections:options.githubReview?JSON.parse(readFileSync(options.githubReview,"utf8")):[],
          ...(options.controller?{controller:await identity("kars-controller")}:{ }),
          ...(options.bridgeConsumers?{bridgeConsumers:{bff:await identity("kars-bridge-bff"),
            gateway:await identity("kars-bridge-teams-gateway"),gatewayReplicas:1}}:{ }),
        }};
      for(const name of options.observe){
        const target=await get(run,"karssandbox",name,options.namespace);
        if(!target)throw new Error("Observation target must already exist");
        (document.spec.observationTargets as Array<{kind:string;namespace:string;name:string;uid:string}>).push({
          kind:"KarsSandbox",namespace:options.namespace,name,uid:target.metadata.uid});
      }
      const reviewedDocument={...document,spec:{...document.spec,privateActivation:await previewPrivateActivation(
        run,options.namespace,writers,document.spec.observationTargets,options.privateRoot,
        options.privateControllerProfile,options.privateConsumer)}};
      await validateGrantDocument(run,reviewedDocument);
      console.log(JSON.stringify(reviewedDocument,null,2));
    });
  command.command("apply").argument("<reviewed-file>").option("--context <context>")
    .action(async(file,options)=>{
      const run=execute(options.context);
      const document=JSON.parse(readFileSync(file,"utf8"));
      await applyReviewedGrant(run,document);
      console.log("Reviewed credential grant recorded; wait for its current Ready condition before using the private adapter.");
    });
  command.command("bootstrap-store").requiredOption("--namespace <namespace>").requiredOption("--name <name>")
    .requiredOption("--purpose <purpose>").option("--context <context>").option("--dry-run")
    .action(async options=>{
      const run=execute(options.context);
      const probe=options.purpose==="providers"?"COPILOT_GITHUB_TOKEN":options.purpose==="foundry"?"FOUNDRY_API_KEY":
        options.purpose==="github-app"?"GITHUB_APP_ID":options.purpose==="github-connection"?"GITHUB_TOKEN":
        options.purpose==="teams"?"client-id":options.purpose==="controller-settings"?"configuration":"API_KEY";
      if(!storeKey(options.purpose,options.name,probe))throw new Error("Store name/purpose is not supported");
      if((await run(["auth","can-i","manage",`${resource}/workspace`,"-n",options.namespace])).trim()!=="yes")
        throw new Error("Explicit credential-grant operator permission is required");
      if(!await get(run,"namespace",options.namespace))throw new Error("Namespace must already exist");
      if(await get(run,"secret",options.name,options.namespace))throw new Error("Existing store preserved; preview its actual UID instead");
      const object={apiVersion:"v1",kind:"Secret",type:"Opaque",metadata:{name:options.name,namespace:options.namespace}};
      if(options.dryRun)console.log(JSON.stringify(object,null,2));
      else {
        const created=JSON.parse(await run(["create","-f","-","-o","json"],JSON.stringify(object)));
        console.log(JSON.stringify({name:created.metadata?.name,namespace:created.metadata?.namespace,
          uid:created.metadata?.uid,resourceVersion:created.metadata?.resourceVersion},null,2));
      }
    });
  return command;
}
