// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFile } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { copyFileSync, cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { loadYaml } from "@kubernetes/client-node";
import { describe, expect, it } from "vitest";

const exec=promisify(execFile);
const root=fileURLToPath(new URL("../../",import.meta.url));
const chart=join(root,"deploy/helm/kars-bridge");
const oldValues=fileURLToPath(new URL("./fixtures/values-10505214.yaml",import.meta.url));

interface LiveFixture { resources:Record<string,any>; values:Record<string,any> }

async function legacyRender(args:string[],lookup=false,protectedConsumer?:{name:string;epoch:boolean},fixture?:LiveFixture):Promise<{objects:any[];calls:string[]}> {
  const directory=join(root,`.chart-upgrade-${randomUUID()}`);
  mkdirSync(directory);
  copyFileSync(join(chart,"Chart.yaml"),join(directory,"Chart.yaml"));
  cpSync(join(chart,"templates"),join(directory,"templates"),{recursive:true});
  // Replace chart defaults, not merely --values overlay: this models Helm
  // --reuse-values where NEW parent maps are actually absent.
  copyFileSync(oldValues,join(directory,"values.yaml"));
  const calls:string[]=[];
  const api=createServer((request,response)=>{
    const path=new URL(request.url!,"http://localhost").pathname;
    calls.push(`${request.method} ${path}`);
    const resources=(entries:Array<[string,string,boolean]>)=>entries.map(([name,kind,namespaced])=>({
      name,singularName:"",namespaced,kind,verbs:["get","list"]}));
    const groups=["apps","rbac.authorization.k8s.io","networking.k8s.io","kars.azure.com"];
    const discovery:Record<string,Array<[string,string,boolean]>>={
      "/api/v1":[["namespaces","Namespace",false],["services","Service",true],["serviceaccounts","ServiceAccount",true],
        ["configmaps","ConfigMap",true],["secrets","Secret",true]],
      "/apis/apps/v1":[["deployments","Deployment",true]],
      "/apis/kars.azure.com/v1alpha1":[["karssandboxes","KarsSandbox",true]],
      "/apis/rbac.authorization.k8s.io/v1":[["roles","Role",true],["rolebindings","RoleBinding",true],
        ["clusterroles","ClusterRole",false],["clusterrolebindings","ClusterRoleBinding",false]],
      "/apis/networking.k8s.io/v1":[["networkpolicies","NetworkPolicy",true],["ingresses","Ingress",true]],
    };
    const value=fixture&&Object.hasOwn(fixture.resources,path)?fixture.resources[path]:
      path==="/version"?{major:"1",minor:"32",gitVersion:"v1.32.0"}:
      path==="/api"?{apiVersion:"v1",kind:"APIVersions",versions:["v1"],serverAddressByClientCIDRs:[]}:
      path==="/apis"?{apiVersion:"v1",kind:"APIGroupList",groups:groups.map(name=>({name,
        versions:[{groupVersion:`${name}/${name==="kars.azure.com"?"v1alpha1":"v1"}`,version:name==="kars.azure.com"?"v1alpha1":"v1"}],preferredVersion:{groupVersion:`${name}/${name==="kars.azure.com"?"v1alpha1":"v1"}`,version:name==="kars.azure.com"?"v1alpha1":"v1"}}))}:
      discovery[path]?{apiVersion:"v1",kind:"APIResourceList",groupVersion:path==="/api/v1"?"v1":path.slice("/apis/".length),
        resources:resources(discovery[path]!)}:
      protectedConsumer&&path===`/apis/apps/v1/namespaces/kars-system/deployments/${protectedConsumer.name}`?
        {apiVersion:"apps/v1",kind:"Deployment",metadata:{name:protectedConsumer.name,namespace:"kars-system",uid:"private-consumer"},
          spec:{template:{metadata:{annotations:protectedConsumer.epoch?{"kars.azure.com/private-epoch":"retained"}:{}}}}}:
      path==="/api/v1/namespaces/kars-system"?{apiVersion:"v1",kind:"Namespace",metadata:{name:"kars-system",uid:"existing",
        labels:{customer:"retained"},annotations:{"meta.helm.sh/release-name":"kars-bridge",
          "meta.helm.sh/release-namespace":"kars-system",customer:"retained",
          ...(protectedConsumer&&!protectedConsumer.epoch?{"kars.azure.com/private-parent-private-consumer":"retained"}:{})}}}:null;
    response.writeHead(value?200:404,{"content-type":"application/json"});
    response.end(JSON.stringify(value??{apiVersion:"v1",kind:"Status",code:404,reason:"NotFound"}));
  });
  try {
    const extra:string[]=[];
    if(fixture){
      const values=join(directory,"selected-values.json");
      writeFileSync(values,JSON.stringify(fixture.values));
      extra.push("--values",values);
    }
    if(lookup){
      await new Promise<void>((resolve)=>api.listen(0,"127.0.0.1",resolve));
      const address=api.address();
      if(!address||typeof address==="string")throw new Error("test API did not bind");
      const config=join(directory,"kubeconfig");
      writeFileSync(config,JSON.stringify({apiVersion:"v1",kind:"Config",clusters:[{name:"fixture",cluster:{server:`http://127.0.0.1:${address.port}`}}],
        users:[{name:"fixture",user:{}}],contexts:[{name:"fixture",context:{cluster:"fixture",user:"fixture"}}],"current-context":"fixture"}));
      extra.push("--dry-run=server","--disable-openapi-validation","--kubeconfig",config);
    }
    const {stdout}=await exec("helm",["template","kars-bridge",directory,"--namespace","kars-system","--is-upgrade",...extra,...args],
      {timeout:15_000,maxBuffer:4*1024*1024,env:{...process.env,HOME:directory,HELM_CACHE_HOME:join(directory,"cache"),
        HELM_CONFIG_HOME:join(directory,"config"),HELM_DATA_HOME:join(directory,"data")}});
    const objects=stdout.split(/^---\s*$/m)
      .filter(doc=>doc.split("\n").some(line=>line.trim()&&!line.trimStart().startsWith("#")))
      .map(doc=>loadYaml(doc) as any).filter(Boolean);
    return {objects,calls};
  } finally {
    if(api.listening)await new Promise<void>((resolve,reject)=>api.close(error=>error?reject(error):resolve()));
    rmSync(directory,{recursive:true,force:true});
  }
}

describe("BASE105 private release-value compatibility",()=>{
  it("actually omits the new maps in its historical values fixture",()=>{
    const values=loadYaml(readFileSync(oldValues,"utf8")) as any;
    expect(values.core).toBeUndefined();
    expect(values.web.oidc).toBeUndefined();
    expect(values.networkPolicy.observations).toBeUndefined();
  });
  it.each([[],["--set","core.namespace="]].map(args=>({args})))("defaults BFF/web workspace and observations off with old values $args",async({args})=>{
    const {objects}=await legacyRender(args);
    expect(objects.some(item=>item.metadata?.name==="kars-bridge-observation-egress")).toBe(false);
    const webPolicy=objects.find(item=>item.kind==="NetworkPolicy"&&item.metadata.name==="kars-bridge-web");
    expect(webPolicy.spec.egress.some((rule:any)=>rule.to?.some((peer:any)=>
      peer.namespaceSelector?.matchLabels?.["kubernetes.io/metadata.name"]==="kars-local-idp"))).toBe(false);
    for(const name of ["kars-bridge-bff","kars-bridge-web"]){
      const env=objects.find(item=>item.kind==="Deployment"&&item.metadata.name===name).spec.template.spec.containers[0].env;
      expect(env.find((item:any)=>item.name==="BRIDGE_DEFAULT_NAMESPACE").value).toBe("kars-system");
      if(name.endsWith("bff"))expect(env.find((item:any)=>item.name==="BRIDGE_CORE_NAMESPACE").value).toBe("kars-system");
    }
    const gateway=objects.find(item=>item.kind==="Deployment"&&item.metadata.name==="kars-bridge-teams-gateway");
    expect(gateway.spec.template.spec.containers[0].env.find((item:any)=>item.name==="TEAMS_WATCH_NAMESPACE").value).toBe("kars-system");
    const binding=objects.find(item=>item.kind==="RoleBinding"&&item.metadata.name==="kars-bridge-kars-system-teams-gateway-core-read");
    expect(binding.metadata.namespace).toBe("kars-system");
    expect(binding.roleRef.name).toBe("kars-bridge-kars-system-teams-gateway-core-read");
  });
  it.each(["routerApiServerIngress","dexApiServerIngress"])("rejects explicitly invalid historical %s values",async field=>{
    await expect(legacyRender(["--set-string",`networkPolicy.${field}=false`])).rejects.toThrow("must be a boolean");
  });
  it("executes Helm lookup and preserves a formerly-owned namespace with default flags",async()=>{
    const {objects,calls}=await legacyRender([],true);
    expect(calls).toContain("GET /api/v1/namespaces/kars-system");
    expect(calls.every(call=>call.startsWith("GET "))).toBe(true);
    const namespace=objects.find(item=>item.kind==="Namespace"&&item.metadata.name==="kars-system");
    expect(namespace.metadata.annotations["helm.sh/resource-policy"]).toBe("keep");
    expect(namespace.metadata.annotations.customer).toBe("retained");
    expect(namespace.metadata.labels.customer).toBe("retained");
    expect(objects.some(item=>item.metadata?.name==="kars-bridge-observation-egress")).toBe(false);
  });
  it.each([
    {name:"kars-bridge-bff",epoch:true},
    {name:"kars-bridge-bff",epoch:false},
    {name:"kars-bridge-teams-gateway",epoch:true},
    {name:"kars-bridge-teams-gateway",epoch:false},
  ])("refuses a private-bound $name even without a template epoch (epoch present: $epoch)",async consumer=>{
    await expect(legacyRender([],true,consumer)).rejects.toThrow("KARS_PRIVATE_CONSUMER_UPGRADE_BLOCKED");
  });
});

const prefix="kars.azure.com/private-";
const deploymentPath=(name:string)=>`/apis/apps/v1/namespaces/kars-system/deployments/${name}`;
const namespacePath="/api/v1/namespaces/kars-system";
function canonical(value:any):string {
  if(Array.isArray(value))return `[${value.map(canonical).join(",")}]`;
  if(value&&typeof value==="object")return `{${Object.keys(value).sort().map(key=>`${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}
const digest=(value:any)=>createHash("sha256").update(canonical(value)).digest("hex");
const identify=({name,uid}:any)=>({name,uid});
function activationBinding(activation:any):string {
  const root=activation.root;
  return digest({contract:activation.contract,bundleRevision:activation.bundleRevision,profile:activation.profile,controllerUids:activation.controllerUids,
    root:{namespace:identify(root.namespace),account:identify(root.account),deployment:identify(root.deployment),
      templateDigest:root.templateDigest,replicaIntent:root.replicaIntent,
      budget:root.budgetTls?{namespace:identify(root.budgetTls.namespace),secret:identify(root.budgetTls.secret)}:null},
    namespaces:activation.namespaces.map((scope:any)=>({namespace:identify(scope.namespace),
      consumers:scope.consumers.map((consumer:any)=>({kind:consumer.kind,object:identify(consumer.object),templateDigest:consumer.templateDigest}))
        .sort((a:any,b:any)=>canonical(a).localeCompare(canonical(b)))}))
      .sort((a:any,b:any)=>a.namespace.name.localeCompare(b.namespace.name))});
}

async function qualifiedFixture(epoch=true,mutate?:(live:any,key:string,values:Record<string,any>)=>void,teamsEnabled=false):Promise<LiveFixture> {
  const {objects}=await legacyRender(teamsEnabled?["--set","teamsGateway.enabled=true"]:[]);
  const fixture:LiveFixture={resources:{},values:{teamsGateway:{enabled:teamsEnabled}}};
  const consumers:any[]=[];
  const scopeEpoch="e".repeat(64);
  for(const [key,name] of [["bff","kars-bridge-bff"],["teamsGateway","kars-bridge-teams-gateway"]]){
    const live=structuredClone(objects.find(item=>item.kind==="Deployment"&&item.metadata.name===name));
    Object.assign(live.metadata,{uid:`${key}-uid`,resourceVersion:`${key}-version`,annotations:{
      "meta.helm.sh/release-name":"kars-bridge","meta.helm.sh/release-namespace":"kars-system"}});
    Object.assign(live.spec,{revisionHistoryLimit:10,progressDeadlineSeconds:600,strategy:{type:"RollingUpdate",rollingUpdate:{maxSurge:"25%",maxUnavailable:"25%"}}});
    live.spec.template.metadata.creationTimestamp=null;
    Object.assign(live.spec.template.spec,{serviceAccount:live.spec.template.spec.serviceAccountName,
      restartPolicy:"Always",dnsPolicy:"ClusterFirst",schedulerName:"default-scheduler",terminationGracePeriodSeconds:30});
    for(const container of live.spec.template.spec.containers){
      Object.assign(container,{terminationMessagePath:"/dev/termination-log",terminationMessagePolicy:"File"});
      for(const port of container.ports)port.protocol="TCP";
      for(const probe of [container.readinessProbe,container.livenessProbe]){
        Object.assign(probe,{successThreshold:1,failureThreshold:3});
        probe.timeoutSeconds??=1;
        probe.httpGet.scheme="HTTP";
      }
    }
    fixture.values[key!]??={};
    mutate?.(live,key!,fixture.values);
    consumers.push({kind:"Deployment",object:{name,uid:live.metadata.uid,resourceVersion:live.metadata.resourceVersion},templateDigest:digest(live.spec.template)});
    if(epoch)live.spec.template.metadata.annotations={[`${prefix}epoch`]:scopeEpoch};
    fixture.resources[deploymentPath(name!)]=live;
    fixture.values[key!].privateContinuity={uid:live.metadata.uid,resourceVersion:live.metadata.resourceVersion,replicas:live.spec.replicas};
  }
  const rootLive={apiVersion:"apps/v1",kind:"Deployment",metadata:{name:"kars-controller",namespace:"kars-system",uid:"controller-uid",resourceVersion:"root-version"},
    spec:{replicas:1,template:{metadata:{labels:{app:"controller"}},spec:{serviceAccountName:"kars-controller",containers:[{name:"controller",image:"controller:latest"}]}}}};
  fixture.resources[deploymentPath("kars-controller")]=rootLive;
  consumers.push({kind:"Deployment",object:{name:rootLive.metadata.name,uid:rootLive.metadata.uid,resourceVersion:rootLive.metadata.resourceVersion},templateDigest:digest(rootLive.spec.template)});
  fixture.resources["/api/v1/namespaces/kars-system/serviceaccounts/kars-controller"]={apiVersion:"v1",kind:"ServiceAccount",
    metadata:{name:"kars-controller",namespace:"kars-system",uid:"account-uid",resourceVersion:"account-version"}};
  const identity={name:"kars-system",uid:"existing",resourceVersion:"namespace-version"};
  const activation={contract:"kars.azure.com/private-consumption/v1",phase:"qualified",bundleRevision:"b".repeat(64),
    profile:"kcm-certificate",controllerUids:{},root:{namespace:identity,account:{name:"kars-controller",uid:"account-uid",resourceVersion:"account-version"},
      deployment:{name:"kars-controller",uid:"controller-uid",resourceVersion:"root-version"},templateDigest:digest(rootLive.spec.template),replicaIntent:1},
    namespaces:[{namespace:identity,consumers,epoch:scopeEpoch}]};
  const binding=activationBinding(activation);
  const retirement=canonical({version:1,phase:"restoring",replicaIntent:1,attempt:"a".repeat(64),binding,
    originalVersion:"original-version",pauseRoot:true,captured:{existing:[]},exposedKeys:[]});
  const fields:Record<string,string>={
    "meta.helm.sh/release-name":"kars-bridge","meta.helm.sh/release-namespace":"kars-system",customer:"retained",
    [`${prefix}enabled`]:"true",[`${prefix}state`]:"Qualified",[`${prefix}namespace-uid`]:identity.uid,
    [`${prefix}root-namespace`]:identity.name,[`${prefix}root-namespace-uid`]:identity.uid,
    [`${prefix}root-account`]:activation.root.account.name,[`${prefix}root-uid`]:activation.root.account.uid,
    [`${prefix}root-user`]:"system:serviceaccount:kars-system:kars-controller",
    [`${prefix}root-deployment`]:activation.root.deployment.name,[`${prefix}root-deployment-uid`]:activation.root.deployment.uid,
    [`${prefix}root-template-digest`]:activation.root.templateDigest,[`${prefix}bundle-revision`]:activation.bundleRevision,
    [`${prefix}profile`]:activation.profile,[`${prefix}epoch`]:scopeEpoch,
    [`${prefix}root-retirement`]:canonical({version:2,retirement,retirementDigest:createHash("sha256").update(retirement).digest("hex"),activation}),
  };
  for(const consumer of consumers)fields[`${prefix}parent-${consumer.object.uid}`]=scopeEpoch;
  fixture.resources[namespacePath]={apiVersion:"v1",kind:"Namespace",metadata:{...identity,labels:{customer:"retained"},annotations:fields}};
  return fixture;
}

function changeProof(fixture:LiveFixture,mutate:(proof:any)=>void):void {
  const fields=fixture.resources[namespacePath].metadata.annotations;
  const proof=JSON.parse(fields[`${prefix}root-retirement`]);
  mutate(proof);
  proof.retirementDigest=createHash("sha256").update(proof.retirement).digest("hex");
  fields[`${prefix}root-retirement`]=canonical(proof);
}

function withBudget(fixture:LiveFixture):void {
  changeProof(fixture,proof=>{
    const activation=proof.activation;
    const budget={namespace:activation.root.namespace,secret:{name:"budget-tls",uid:"tls-uid",resourceVersion:"tls-version"},keyDigest:"c".repeat(64)};
    activation.root.budgetTls=budget;
    proof.retirement=canonical({...JSON.parse(proof.retirement),binding:activationBinding(activation),
      baseline:{secretUid:"tls-uid",resourceVersion:"old-tls-version",keyDigest:"d".repeat(64)},exposedKeys:["d".repeat(64)]});
    fixture.resources["/api/v1/namespaces/kars-system/secrets/budget-tls"]={apiVersion:"v1",kind:"Secret",type:"kubernetes.io/tls",metadata:budget.secret};
    for(const [key,value] of Object.entries({"budget-namespace":"kars-system","budget-namespace-uid":"existing",
      "budget-tls-name":"budget-tls","budget-tls-uid":"tls-uid","budget-tls-version":"tls-version","budget-key":budget.keyDigest,
      "budget-qualified-bundle":activation.bundleRevision,"budget-qualified-key":budget.keyDigest,"budget-qualified-secret":"tls-uid"})){
      fixture.resources[namespacePath].metadata.annotations[`${prefix}${key}`]=value;
    }
  });
}

async function controllerRolloutFixture() {
  const fixture=await qualifiedFixture();
  const epoch="a".repeat(64);
  const namespace={name:"kars-sre",uid:"sre-namespace",resourceVersion:"1"};
  const identity={name:"sre",uid:"sre-deployment",resourceVersion:"1"};
  const template={metadata:{labels:{app:"sre"}},spec:{containers:[{name:"router",image:"router:latest"}]}};
  const consumer={kind:"Deployment",object:identity,templateDigest:digest(template)};
  const current:any={apiVersion:"apps/v1",kind:"Deployment",metadata:{...identity,namespace:namespace.name},
    spec:{replicas:1,template:structuredClone(template)}};
  current.spec.template.metadata.annotations={
    [`${prefix}epoch`]:epoch,"kars.azure.com/inference-providers-version":"new-provider-revision",
  };
  const hashed=structuredClone(current.spec.template);
  delete hashed.metadata.annotations[`${prefix}epoch`];
  const receipt:any={epoch,baseline:consumer.templateDigest,structure:digest(template),template:digest(hashed),sandbox_uid:"sre-sandbox",generation:2};
  const annotations:Record<string,string>={...fixture.resources[namespacePath].metadata.annotations};
  for(const key of Object.keys(annotations))if(key.startsWith(`${prefix}parent-`)||key===`${prefix}root-retirement`)delete annotations[key];
  Object.assign(annotations,{[`${prefix}namespace-uid`]:namespace.uid,[`${prefix}epoch`]:epoch,
    [`${prefix}parent-${identity.uid}`]:epoch,"kars.azure.com/namespace-claim-version":"v1",
    "kars.azure.com/sandbox-name":"sre","kars.azure.com/sandbox-namespace":"kars-system","kars.azure.com/sandbox-uid":"sre-sandbox"});
  const source:any={apiVersion:"kars.azure.com/v1alpha1",kind:"KarsSandbox",metadata:{name:"sre",uid:"sre-sandbox",namespace:"kars-system",generation:2,
    annotations:{"kars.azure.com/namespace-uid":namespace.uid}}};
  const scope:any={apiVersion:"v1",kind:"Namespace",metadata:{...namespace,annotations}};
  fixture.resources["/api/v1/namespaces/kars-sre"]=scope;
  fixture.resources["/apis/apps/v1/namespaces/kars-sre/deployments/sre"]=current;
  fixture.resources["/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre"]=source;
  changeProof(fixture,proof=>{
    proof.activation.namespaces.push({namespace,epoch,consumers:[consumer]});
    const retirement=JSON.parse(proof.retirement);
    retirement.captured[namespace.uid]=[];
    retirement.binding=activationBinding(proof.activation);
    proof.retirement=canonical(retirement);
  });
  const stamp=()=>{annotations[`${prefix}reconciled-${identity.uid}`]=JSON.stringify(receipt);};
  stamp();
  return {fixture,receipt,scope,current,source,annotations,stamp};
}

describe("protected Bridge no-op continuity",()=>{
  it("preserves Bridge execution after a projector-verified provider rollout without revising the migration",async()=>{
    const {fixture}=await controllerRolloutFixture();
    fixture.values.web={image:{tag:"reviewed-web-update"}};
    const {objects,calls}=await legacyRender([],true,undefined,fixture);
    for(const name of ["kars-bridge-bff","kars-bridge-teams-gateway"])
      expect(objects.find(item=>item.kind==="Deployment"&&item.metadata.name===name).spec).toEqual(fixture.resources[deploymentPath(name)].spec);
    expect(calls).toContain("GET /apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre");
    expect(calls.every(call=>call.startsWith("GET "))).toBe(true);
  });
  it.each(["baseline","structure","template","epoch","sandbox_uid","generation","extra","missing","image","namespace","sandbox","source-generation","parent","deployment","claim","prestage","owner","bound-namespace","deleting","malformed"]) (
    "rejects changed rollout authority: %s",async fault=>{
      const f=await controllerRolloutFixture();
      if(["baseline","structure","template","epoch","sandbox_uid"].includes(fault))f.receipt[fault]="changed";
      if(fault==="generation")f.receipt.generation=1;
      if(fault==="extra")f.receipt.ignored=true;
      if(fault==="image")f.current.spec.template.spec.containers[0].image="changed";
      if(fault==="namespace")f.scope.metadata.uid="replaced";
      if(fault==="sandbox")f.source.metadata.uid="replaced";
      if(fault==="source-generation")f.source.metadata.generation++;
      if(fault==="parent")f.annotations[`${prefix}parent-sre-deployment`]="changed";
      if(fault==="deployment")f.current.metadata.uid="replaced";
      if(fault==="claim")f.annotations["kars.azure.com/namespace-claim-version"]="changed";
      if(fault==="prestage")f.annotations["kars.azure.com/namespace-prestage"]="";
      if(fault==="owner")f.scope.metadata.ownerReferences=[{uid:"foreign"}];
      if(fault==="bound-namespace")f.source.metadata.annotations["kars.azure.com/namespace-uid"]="changed";
      if(fault==="deleting")f.source.metadata.deletionTimestamp="2026-10-29T00:00:00Z";
      f.stamp();
      if(fault==="missing")delete f.annotations[`${prefix}reconciled-sre-deployment`];
      if(fault==="malformed")f.annotations[`${prefix}reconciled-sre-deployment`]="{";
      await expect(legacyRender([],true,undefined,f.fixture)).rejects.toThrow("KARS_PRIVATE_CONSUMER_UPGRADE_BLOCKED");
    },
  );
  it("does not invent a private consumer for an unbound existing gateway",async()=>{
    const fixture=await qualifiedFixture();
    const gateway=fixture.resources[deploymentPath("kars-bridge-teams-gateway")];
    delete gateway.spec.template.metadata.annotations;
    delete fixture.values.teamsGateway.privateContinuity;
    delete fixture.resources[namespacePath].metadata.annotations[`${prefix}parent-${gateway.metadata.uid}`];
    changeProof(fixture,proof=>{
      proof.activation.namespaces[0].consumers=proof.activation.namespaces[0].consumers.filter((consumer:any)=>consumer.object.uid!==gateway.metadata.uid);
      proof.retirement=canonical({...JSON.parse(proof.retirement),binding:activationBinding(proof.activation)});
    });
    const {objects}=await legacyRender([],true,undefined,fixture);
    expect(objects.find(item=>item.kind==="Deployment"&&item.metadata.name==="kars-bridge-bff").metadata.uid).toBe("bff-uid");
    expect(objects.find(item=>item.kind==="Deployment"&&item.metadata.name==="kars-bridge-teams-gateway").metadata.uid).toBeUndefined();
  });
  it("requires unchanged rotated TLS metadata when the retained root has a budget",async()=>{
    const fixture=await qualifiedFixture();
    withBudget(fixture);
    const {objects}=await legacyRender([],true,undefined,fixture);
    expect(objects.find(item=>item.kind==="Deployment"&&item.metadata.name==="kars-bridge-bff").spec)
      .toEqual(fixture.resources[deploymentPath("kars-bridge-bff")].spec);
    fixture.resources["/api/v1/namespaces/kars-system/secrets/budget-tls"].metadata.resourceVersion="changed";
    await expect(legacyRender([],true,undefined,fixture)).rejects.toThrow("KARS_PRIVATE_CONSUMER_UPGRADE_BLOCKED");
  });
  it("checks other scopes in the retained root activation, not only the Bridge namespace",async()=>{
    const fixture=await qualifiedFixture();
    const otherNamespace={name:"kars-other",uid:"other-namespace-uid",resourceVersion:"other-namespace-version"};
    const otherConsumer={name:"other-consumer",uid:"other-consumer-uid",resourceVersion:"other-consumer-version"};
    const template={metadata:{labels:{app:"other-consumer"}},spec:{containers:[{name:"other",image:"other:latest"}]}};
    const annotations={...fixture.resources[namespacePath].metadata.annotations};
    for(const key of Object.keys(annotations))if(key.startsWith(`${prefix}parent-`)||key===`${prefix}root-retirement`)delete annotations[key];
    annotations[`${prefix}namespace-uid`]=otherNamespace.uid;
    annotations[`${prefix}epoch`]="a".repeat(64);
    annotations[`${prefix}parent-${otherConsumer.uid}`]="a".repeat(64);
    const otherPath="/api/v1/namespaces/kars-other";
    fixture.resources[otherPath]={apiVersion:"v1",kind:"Namespace",metadata:{...otherNamespace,annotations}};
    fixture.resources["/apis/apps/v1/namespaces/kars-other/deployments/other-consumer"]={apiVersion:"apps/v1",kind:"Deployment",
      metadata:{...otherConsumer,namespace:"kars-other"},spec:{replicas:1,template}};
    changeProof(fixture,proof=>{
      proof.activation.namespaces.push({namespace:otherNamespace,epoch:"a".repeat(64),
        consumers:[{kind:"Deployment",object:otherConsumer,templateDigest:digest(template)}]});
      const retirement=JSON.parse(proof.retirement);
      retirement.captured[otherNamespace.uid]=[];
      retirement.binding=activationBinding(proof.activation);
      proof.retirement=canonical(retirement);
    });
    await expect(legacyRender([],true,undefined,fixture)).resolves.toHaveProperty("objects");
    annotations[`${prefix}state`]="Pending";
    await expect(legacyRender([],true,undefined,fixture)).rejects.toThrow("KARS_PRIVATE_CONSUMER_UPGRADE_BLOCKED");
    annotations[`${prefix}state`]="Qualified";
    template.spec.containers[0]!.image="unsettled-control-material";
    await expect(legacyRender([],true,undefined,fixture)).rejects.toThrow("KARS_PRIVATE_CONSUMER_UPGRADE_BLOCKED");
  });
  it("refuses an offline fence without live qualification evidence",async()=>{
    const fixture=await qualifiedFixture();
    await expect(legacyRender([],false,undefined,fixture)).rejects.toThrow("KARS_PRIVATE_CONSUMER_UPGRADE_BLOCKED");
  });
  it("retains a no-op with an already-active Teams gateway",async()=>{
    const fixture=await qualifiedFixture(true,undefined,true);
    const {objects}=await legacyRender([],true,undefined,fixture);
    for(const name of ["kars-bridge-bff","kars-bridge-teams-gateway"]){
      const live=fixture.resources[deploymentPath(name)];
      expect(live.spec.replicas).toBeGreaterThan(0);
      expect(objects.find(item=>item.kind==="Deployment"&&item.metadata.name===name).spec).toEqual(live.spec);
    }
  });
  it("records an already-qualified image without restoring the old Helm image",async()=>{
    const fixture=await qualifiedFixture(true,(live,key,values)=>{
      if(key!=="bff")return;
      values.bff.image={tag:"qualified"};
      live.spec.template.spec.containers[0].image=live.spec.template.spec.containers[0].image.replace(/:[^:]+$/u,":qualified");
    });
    const {objects}=await legacyRender([],true,undefined,fixture);
    expect(objects.find(item=>item.kind==="Deployment"&&item.metadata.name==="kars-bridge-bff").spec)
      .toEqual(fixture.resources[deploymentPath("kars-bridge-bff")].spec);
  });
  it("accepts exact API quantity canonicalization without changing the live template",async()=>{
    const fixture=await qualifiedFixture(true,(live,key,values)=>{
      if(key!=="bff")return;
      live.spec.template.spec.containers[0].resources={requests:{cpu:"100m",memory:"128Mi"},limits:{cpu:"1",memory:"1Gi"}};
      values.bff.resources={requests:{cpu:"0.100",memory:"134217728"},limits:{cpu:"1000m",memory:"1024Mi"}};
    });
    const {objects}=await legacyRender([],true,undefined,fixture);
    expect(objects.find(item=>item.kind==="Deployment"&&item.metadata.name==="kars-bridge-bff").spec)
      .toEqual(fixture.resources[deploymentPath("kars-bridge-bff")].spec);
  });
  it.each([
    ["strategy",(live:any)=>{live.spec.strategy.rollingUpdate.maxSurge="50%";}],
    ["probe",(live:any)=>{live.spec.template.spec.containers[0].readinessProbe.successThreshold=2;}],
    ["security",(live:any)=>{live.spec.template.spec.securityContext.runAsUser=1002;}],
    ["scheduler",(live:any)=>{live.spec.template.spec.schedulerName="custom";}],
  ] as Array<[string,(live:any)=>void]>)("does not hide qualified non-default %s intent",async(_name,mutate)=>{
    const fixture=await qualifiedFixture(true,(live,key)=>{if(key==="bff")mutate(live);});
    await expect(legacyRender([],true,undefined,fixture)).rejects.toThrow("KARS_PRIVATE_CONSUMER_UPGRADE_BLOCKED");
  });
  it.each([true,false])("preserves exact qualified live execution and UID/RV fences, with epoch=%s",async epoch=>{
    const fixture=await qualifiedFixture(epoch);
    fixture.values.web={image:{tag:"reviewed-web-update"}};
    const {objects,calls}=await legacyRender([],true,undefined,fixture);
    for(const name of ["kars-bridge-bff","kars-bridge-teams-gateway"]){
      const result=objects.find(item=>item.kind==="Deployment"&&item.metadata.name===name);
      const live=fixture.resources[deploymentPath(name)];
      expect(result.spec).toEqual(live.spec);
      expect(result.metadata.uid).toBe(live.metadata.uid);
      expect(result.metadata.resourceVersion).toBe(live.metadata.resourceVersion);
    }
    const namespace=objects.find(item=>item.kind==="Namespace");
    expect(namespace.metadata.uid).toBe("existing");
    expect(namespace.metadata.resourceVersion).toBe("namespace-version");
    expect(namespace.metadata.annotations).toEqual({...fixture.resources[namespacePath].metadata.annotations,"helm.sh/resource-policy":"keep"});
    expect(objects.find(item=>item.metadata.name==="kars-bridge-web"&&item.kind==="Deployment").spec.template.spec.containers[0].image).toContain(":reviewed-web-update");
    expect(calls.every(call=>call.startsWith("GET "))).toBe(true);
  });
  it.each([
    ["missing fence",(f:LiveFixture)=>{delete f.values.bff.privateContinuity;}],
    ["extra fence fields",(f:LiveFixture)=>{f.values.bff.privateContinuity.disable=true;}],
    ["replica string",(f:LiveFixture)=>{f.values.bff.privateContinuity.replicas="1";}],
    ["root execution drift",(f:LiveFixture)=>{f.resources[deploymentPath("kars-controller")].spec.template.spec.containers[0].image="changed";}],
    ["root still paused",(f:LiveFixture)=>{f.resources[deploymentPath("kars-controller")].spec.replicas=0;}],
    ["root identity replacement",(f:LiveFixture)=>{f.resources["/api/v1/namespaces/kars-system/serviceaccounts/kars-controller"].metadata.uid="replacement";}],
    ["retirement binding",(f:LiveFixture)=>changeProof(f,p=>{p.retirement=canonical({...JSON.parse(p.retirement),binding:"f".repeat(64)});})],
    ["retirement Pending",(f:LiveFixture)=>changeProof(f,p=>{p.retirement=canonical({...JSON.parse(p.retirement),phase:"retired"});})],
    ["retirement capture inventory",(f:LiveFixture)=>changeProof(f,p=>{p.retirement=canonical({...JSON.parse(p.retirement),captured:{}});})],
    ["activation Pending",(f:LiveFixture)=>changeProof(f,p=>{p.activation.phase="pending";})],
    ["proof corruption",(f:LiveFixture)=>{f.resources[namespacePath].metadata.annotations[`${prefix}root-retirement`]+=" ";}],
    ["UID fence",(f:LiveFixture)=>{f.values.bff.privateContinuity.uid="replaced";}],
    ["RV fence",(f:LiveFixture)=>{f.values.bff.privateContinuity.resourceVersion="stale";}],
    ["reviewed replicas",(f:LiveFixture)=>{f.values.bff.privateContinuity.replicas=0;}],
    ["requested replicas",(f:LiveFixture)=>{f.values.bff.replicas=0;}],
    ["image",(f:LiveFixture)=>{f.values.bff.image={tag:"unreviewed"};}],
    ["environment",(f:LiveFixture)=>{f.values.bff.extraEnv=[{name:"NEW_AUTHORITY",value:"yes"}];}],
    ["resources",(f:LiveFixture)=>{f.values.bff.resources={limits:{memory:"99Gi"}};}],
    ["identity",(f:LiveFixture)=>{f.values.rbac={serviceAccountName:"other"};}],
    ["volumes",(f:LiveFixture)=>{f.resources[deploymentPath("kars-bridge-bff")].spec.template.spec.volumes=[{name:"extra",emptyDir:{}}];}],
    ["Pending",(f:LiveFixture)=>{f.resources[namespacePath].metadata.annotations[`${prefix}state`]="Pending";}],
    ["missing proof",(f:LiveFixture)=>{delete f.resources[namespacePath].metadata.annotations[`${prefix}root-retirement`];}],
    ["wrong namespace UID",(f:LiveFixture)=>{f.resources[namespacePath].metadata.uid="replacement";}],
    ["missing parent",(f:LiveFixture)=>{delete f.resources[namespacePath].metadata.annotations[`${prefix}parent-bff-uid`];}],
    ["extra parent",(f:LiveFixture)=>{f.resources[namespacePath].metadata.annotations[`${prefix}parent-unreviewed`]="e".repeat(64);}],
    ["wrong epoch",(f:LiveFixture)=>{f.resources[deploymentPath("kars-bridge-bff")].spec.template.metadata.annotations[`${prefix}epoch`]="stale";}],
    ["not release-owned",(f:LiveFixture)=>{f.resources[deploymentPath("kars-bridge-bff")].metadata.annotations["meta.helm.sh/release-name"]="other";}],
    ["missing Deployment",(f:LiveFixture)=>{f.resources[deploymentPath("kars-bridge-bff")]=null;}],
    ["gateway activation",(f:LiveFixture)=>{f.values.teamsGateway.enabled=true;}],
    ["gateway image",(f:LiveFixture)=>{f.values.teamsGateway.image={tag:"unreviewed"};}],
    ["publication in progress",(f:LiveFixture)=>{f.resources[namespacePath].metadata.annotations[`${prefix}core-publication`]="owned";}],
  ] as Array<[string,(f:LiveFixture)=>void]>)("rejects %s",async(_name,mutate)=>{
    const fixture=await qualifiedFixture();
    mutate(fixture);
    await expect(legacyRender([],true,undefined,fixture)).rejects.toThrow("KARS_PRIVATE_CONSUMER_UPGRADE_BLOCKED");
  });
});
