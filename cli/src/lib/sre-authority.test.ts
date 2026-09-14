// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe,expect,it,vi } from "vitest";
import { assertDestroySafe,assertRollbackSafe,assertSafeMutation,enroll,preview,waitForAuthority,type Execute } from "./sre-authority.js";
import { stageSource } from "./sre-source.js";
import { stageAuthority } from "./sre-stage.js";
import { readFileSync } from "node:fs";
import { planCoreHelmSchemas } from "./core-helm-schemas.js";
vi.mock("./core-helm-schemas.js", () => ({ planCoreHelmSchemas: vi.fn(async () => async () => ({ schemas: 21, published: true })) }));
vi.mock("./schema-stage.js", () => ({ waitForInstalledCoreSchemas: vi.fn(async () => {}) }));

const actionCrd=readFileSync(new URL("../../../deploy/helm/kars/templates/crd-karssreaction.yaml",import.meta.url),"utf8");

function fixture() {
  const objects:Record<string,any>={
    "namespace//kars-system":{kind:"Namespace",metadata:{name:"kars-system",uid:"control-ns",resourceVersion:"1"}},
    "deployment/kars-system/kars-controller":{kind:"Deployment",metadata:{name:"kars-controller",namespace:"kars-system",uid:"controller",resourceVersion:"1"}},
    "karssandbox/kars-system/sre":{kind:"KarsSandbox",metadata:{name:"sre",namespace:"kars-system",uid:"source",resourceVersion:"1",
      annotations:{"kars.azure.com/namespace-uid":"runtime-ns"}}},
    "namespace//kars-sre":{kind:"Namespace",metadata:{name:"kars-sre",uid:"runtime-ns",resourceVersion:"1",
      annotations:{"kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":"kars-system",
        "kars.azure.com/sandbox-name":"sre","kars.azure.com/sandbox-uid":"source"}}},
    "crd//karssreregistrations.kars.azure.com":{kind:"CustomResourceDefinition",metadata:{name:"karssreregistrations.kars.azure.com",uid:"crd",resourceVersion:"1"}},
  };
  const bindings:any[]=[];
  let use=true;
  const execute=vi.fn<Execute>(async (_file,args,options)=>{
    if(args[0]==="auth")return {stdout:use?"yes":"no"};
    if(args[0]==="get"&&(args[1]==="clusterrolebindings"||args[1]==="rolebindings"))
      return {stdout:JSON.stringify({items:args[1]==="clusterrolebindings"?bindings:[]})};
    if(args[0]==="get"){
      const namespace=args.includes("-n")?args[args.indexOf("-n")+1]:"";
      const object=objects[`${args[1]}/${namespace}/${args[2]}`];
      return {stdout:object?JSON.stringify(object):""};
    }
    if(args[0]==="create"){
      const body=JSON.parse(options.input!);
      objects["karssreregistrations.kars.azure.com//canonical"]={...body,metadata:{...body.metadata,uid:"registration",resourceVersion:"1",generation:1}};
      return {stdout:JSON.stringify(objects["karssreregistrations.kars.azure.com//canonical"])};
    }
    return {stdout:""};
  });
  return {objects,bindings,execute,denyUse:()=>{use=false}};
}

describe("SRE cluster registrar boundary",()=>{
  it("previews only metadata and never enrolls based on labels or a foreign namespace",async()=>{
    const f=fixture();
    const result=await preview(f.execute,"kars-system","kars");
    expect(result.sandbox.uid).toBe("source");
    expect(f.execute.mock.calls.every(([,args])=>args[0]==="get")).toBe(true);
    f.objects["namespace//kars-sre"].metadata.annotations["kars.azure.com/sandbox-namespace"]="foreign";
    await expect(preview(f.execute,"kars-system","kars")).rejects.toThrow(/different|foreign|claim/i);
  });

  it("requires explicit registrar permission and reviewed UIDs",async()=>{
    const f=fixture();
    const spec=await preview(f.execute,"kars-system","kars");
    f.denyUse();
    await expect(enroll(f.execute,spec,{sandboxUid:"source",namespaceUid:"runtime-ns",binding:[]},false)).rejects.toThrow("registrar");
    expect(f.execute.mock.calls.some(([,args])=>args[0]==="create"||args[0]==="patch")).toBe(false);
  });

  it("requires every legacy binding and exact resource version before enrollment",async()=>{
    const f=fixture();
    f.bindings.push({kind:"ClusterRoleBinding",metadata:{name:"legacy",uid:"binding",resourceVersion:"4"},
      roleRef:{apiGroup:"rbac.authorization.k8s.io",kind:"ClusterRole",name:"kars-sre-reader"},
      subjects:[{kind:"ServiceAccount",name:"sandbox",namespace:"kars-sre"}]});
    const spec=await preview(f.execute,"kars-system","kars");
    await expect(enroll(f.execute,spec,{sandboxUid:"source",namespaceUid:"runtime-ns",binding:[]},false)).rejects.toThrow("Every legacy binding");
    await enroll(f.execute,spec,{sandboxUid:"source",namespaceUid:"runtime-ns",binding:["ClusterRoleBinding//legacy=binding@4"]},false);
    const body=JSON.parse(f.execute.mock.calls.find(([,args])=>args[0]==="create")![2].input!);
    expect(body.kind).toBe("KarsSRERegistration");
    expect(body.metadata.namespace).toBeUndefined();
  });

  it("blocks ordinary mutation and rollback while old grants or authority remain",async()=>{
    const f=fixture();
    f.bindings.push({kind:"ClusterRoleBinding",metadata:{name:"legacy",uid:"binding",resourceVersion:"1"},
      roleRef:{apiGroup:"rbac.authorization.k8s.io",kind:"ClusterRole",name:"kars-sre-reader"},
      subjects:[{kind:"ServiceAccount",name:"sandbox",namespace:"kars-sre"}]});
    await expect(assertSafeMutation(f.execute)).rejects.toThrow("Legacy SRE grants");
    expect(f.execute.mock.calls.every(([,args])=>args[0]==="get")).toBe(true);
    f.bindings.length=0;
    f.objects["karssreregistrations.kars.azure.com//canonical"]={metadata:{name:"canonical",uid:"registration",resourceVersion:"1",generation:1},
      spec:{enabled:true},status:{phase:"Ready",observedGeneration:1}};
    await expect(assertRollbackSafe(f.execute)).rejects.toThrow("Rollback");
    await expect(assertDestroySafe(f.execute)).rejects.toThrow("Retire");
  });

  it("dry-run enrollment performs no mutation",async()=>{
    const f=fixture();
    const spec=await preview(f.execute,"kars-system","kars");
    const result=await enroll(f.execute,spec,{sandboxUid:"source",namespaceUid:"runtime-ns",binding:[]},true);
    expect(JSON.parse(result).spec.sandbox.uid).toBe("source");
    expect(f.execute.mock.calls.some(([,args])=>args[0]==="create"||args[0]==="patch")).toBe(false);
  });

  it.each([false,true])("replaces the full reviewed re-enrollment spec (consumer present: %s)",async hasConsumer=>{
    const f=fixture();
    if(hasConsumer) f.objects["deployment/kars-sre/sre"]={kind:"Deployment",
      metadata:{name:"sre",namespace:"kars-sre",uid:"new-consumer",resourceVersion:"9"}};
    const spec=await preview(f.execute,"kars-system","kars");
    const key="karssreregistrations.kars.azure.com//canonical";
    f.objects[key]={metadata:{name:"canonical",uid:"registration",resourceVersion:"7",generation:2},
      spec:{...spec,enabled:false,legacyConsumer:{namespace:"kars-sre",name:"sre",uid:"retired-consumer",resourceVersion:"2"},
        legacyBindings:[{name:"retired-grant"}]},status:{phase:"Retired",observedGeneration:2}};
    const execute=vi.fn<Execute>(async(file,args,options)=>{
      if(args[0]!=="patch") return f.execute(file,args,options);
      expect(args).toContain("--type=json");
      const operations=JSON.parse(args[args.indexOf("-p")+1]);
      expect(operations).toEqual([
        {op:"test",path:"/metadata/uid",value:"registration"},
        {op:"test",path:"/metadata/resourceVersion",value:"7"},
        {op:"replace",path:"/spec",value:spec},
      ]);
      f.objects[key].spec=structuredClone(operations[2].value);
      return {stdout:""};
    });
    await enroll(execute,spec,{
      sandboxUid:"source",namespaceUid:"runtime-ns",binding:[],
      registrationUid:"registration",resourceVersion:"7",
      ...(hasConsumer?{consumer:"new-consumer@9"}:{}),
    },false);
    expect(f.objects[key].spec).toEqual(spec);
    expect(f.objects[key].spec.legacyConsumer).toEqual(hasConsumer?spec.legacyConsumer:undefined);
    expect(f.objects[key].metadata.uid).toBe("registration");
  });

  it("does not patch re-enrollment against a stale registration identity",async()=>{
    const f=fixture();
    const spec=await preview(f.execute,"kars-system","kars");
    f.objects["karssreregistrations.kars.azure.com//canonical"]={
      metadata:{name:"canonical",uid:"replacement-registration",resourceVersion:"8",generation:1},spec};
    await expect(enroll(f.execute,spec,{
      sandboxUid:"source",namespaceUid:"runtime-ns",binding:[],
      registrationUid:"registration",resourceVersion:"7",
    },false)).rejects.toThrow("reviewed registration UID/resourceVersion");
    expect(f.execute.mock.calls.some(([,args])=>args[0]==="patch")).toBe(false);
  });

  it("refuses to stage over any preexisting source or namespace",async()=>{
    const f=fixture();
    await expect(stageSource(f.execute,"","kars-system","kars")).rejects.toThrow("already exists");
    delete f.objects["karssandbox/kars-system/sre"];
    await expect(stageSource(f.execute,"","kars-system","kars")).rejects.toThrow("Existing kars-sre");
    expect(f.execute.mock.calls.some(([,args])=>args[0]==="create"||args[0]==="patch")).toBe(false);
  });

  it("captures CREATE UID and never converts a racing 409 into adoption",async()=>{
    const f=fixture();
    delete f.objects["karssandbox/kars-system/sre"];
    delete f.objects["namespace//kars-sre"];
    const base=f.execute;
    const execute=vi.fn<Execute>(async(file,args,options)=>{
      if(args[0]==="create")throw new Error("409 AlreadyExists");
      return base(file,args,options);
    });
    const rendered=JSON.stringify({apiVersion:"kars.azure.com/v1alpha1",kind:"KarsSandbox",
      metadata:{name:"sre",namespace:"kars-system"},spec:{runtime:{kind:"Hermes"}}});
    await expect(stageSource(execute,rendered,"kars-system","kars")).rejects.toThrow("409");
    const createIndex=execute.mock.calls.findIndex(([,args])=>args[0]==="create");
    expect(createIndex).toBeGreaterThanOrEqual(0);
    expect(execute.mock.calls.slice(createIndex+1)).toEqual([]);
  });

  it("enrolls the actual newly created source only after its namespace claim converges",async()=>{
    const f=fixture();
    const source=f.objects["karssandbox/kars-system/sre"];
    const runtime=f.objects["namespace//kars-sre"];
    delete f.objects["karssandbox/kars-system/sre"];
    delete f.objects["namespace//kars-sre"];
    const execute=vi.fn<Execute>(async(file,args,options)=>{
      if(args[0]==="create"&&JSON.parse(options.input!).kind==="KarsSandbox") {
        f.objects["karssandbox/kars-system/sre"]=source;
        f.objects["namespace//kars-sre"]=runtime;
        return {stdout:JSON.stringify(source)};
      }
      return f.execute(file,args,options);
    });
    const created=await stageSource(execute,JSON.stringify(source),"kars-system","kars");
    expect(created).toEqual({uid:"source",namespaceUid:"runtime-ns"});
    await enroll(execute,await preview(execute,"kars-system","kars"),{
      sandboxUid:created.uid,namespaceUid:created.namespaceUid,binding:[],
    },false);
    expect(f.objects["karssreregistrations.kars.azure.com//canonical"].spec.sandbox.uid).toBe("source");
    expect(execute.mock.calls.filter(([,args])=>args[0]==="create")).toHaveLength(2);
  });

  it("waits through controller migration without treating transient drain as failure",async()=>{
    vi.useFakeTimers();
    try {
      const f=fixture();
      const key="karssreregistrations.kars.azure.com//canonical";
      f.objects[key]={metadata:{name:"canonical",uid:"registration",resourceVersion:"1",generation:1},
        status:{phase:"Migrating",observedGeneration:1,privacyRevision:"kars.azure.com/sre-privacy/v2"}};
      const pending=waitForAuthority(f.execute,"Ready",3);
      setTimeout(()=>{f.objects[key].status.phase="Ready"},2100);
      await vi.runAllTimersAsync();
      await pending;
    } finally { vi.useRealTimers(); }
  });

  it.each([
    {version:"v3.16.4",dryRun:true}, {version:"v3.16.4",dryRun:false},
    {version:"v4.2.4",dryRun:true}, {version:"v4.2.4",dryRun:false},
  ])("stages an existing release with $version (dry-run: $dryRun)",async({version,dryRun})=>{
    const warn=vi.spyOn(console,"warn").mockImplementation(()=>{});
    try {
      const f=fixture();
      f.objects["deployment/kars-system/kars-controller"].spec={
        template:{spec:{serviceAccountName:"kars-controller"}},
      };
      const execute=vi.fn<Execute>(async(file,args,options)=>{
        if(file==="helm"&&args[0]==="list") {
          if(version.startsWith("v4.")&&args.includes("--all")) {
            throw Object.assign(new Error("Unsupported flag"),{
              exitCode:1,stderr:"Error: unknown flag: --all\n",
            });
          }
          return {stdout:'[{"name":"kars","namespace":"kars-system","status":"pending-upgrade"}]'};
        }
        if(file==="helm"&&args[0]==="version")return {stdout:version};
        if(file==="helm"&&args[0]==="template")return {stdout:actionCrd};
        if(file==="helm"&&args[0]==="upgrade")return {stdout:""};
        return f.execute(file,args,options);
      });
      vi.mocked(planCoreHelmSchemas).mockClear();
      await stageAuthority(execute,"chart","kars-system","kars","new/controller:latest","new/router:latest",dryRun);
      const upgrade=execute.mock.calls.find(([file,args])=>file==="helm"&&args[0]==="upgrade"&&(dryRun||!args.includes("--dry-run=server")));
      expect(upgrade?.[1]).toContain("--reset-then-reuse-values");
      expect(upgrade?.[1]).toContain("sre.authorityStage=true");
      expect(upgrade?.[1]).toContain(dryRun?"--dry-run=server":version.startsWith("v4.")?"--wait=legacy":"--wait");
      expect(execute.mock.calls.some(([,args])=>args[0]==="install")).toBe(false);
      expect(planCoreHelmSchemas).toHaveBeenCalledTimes(1);
    } finally { warn.mockRestore(); }
  });

  it("stages legacy template installations using owned resources and controller CAS without overwriting other env",async()=>{
    const f=fixture();
    const controller=f.objects["deployment/kars-system/kars-controller"];
    controller.spec={template:{spec:{serviceAccountName:"kars-controller",containers:[
      {name:"controller",image:"old/controller:latest",env:[{name:"PRESERVED",value:"setting"}]},
    ]}}};
    const execute=vi.fn<Execute>(async(file,args,options)=>{
      if(file==="helm"&&args[0]==="list")return {stdout:"[]"};
      if(file==="helm")return {stdout:JSON.stringify({kind:"CustomResourceDefinition",
        apiVersion:"apiextensions.k8s.io/v1",metadata:{name:"karssreregistrations.kars.azure.com"},spec:{scope:"Cluster"}})+"\n"+actionCrd};
      return f.execute(file,args,options);
    });
    delete f.objects["crd//karssreregistrations.kars.azure.com"];
    await stageAuthority(execute,"chart","kars-system","kars","new/controller:latest","new/router:latest",false);
    const patch=execute.mock.calls.find(([,args])=>args[0]==="patch")!;
    const body=JSON.parse(patch[1][patch[1].indexOf("-p")+1]);
    expect(body.metadata).toEqual({uid:"controller",resourceVersion:"1"});
    expect(body.spec.template.spec.containers[0].env).toContainEqual({name:"PRESERVED",value:"setting"});
    expect(body.spec.template.spec.containers[0].env).toContainEqual({name:"INFERENCE_ROUTER_IMAGE",value:"new/router:latest"});
    expect(execute.mock.calls.some(([,args])=>args.includes("--force-conflicts"))).toBe(false);
  });

  it.each([
    {apiGroups:[""],resources:["secrets"],verbs:["watch"]},
    {apiGroups:["*"],resources:["*"],verbs:["watch"]},
    {apiGroups:[""],resources:["secrets"],verbs:["*"]},
    {apiGroups:[""],resources:["pods","secrets"],verbs:["get","watch"]},
  ])("rejects group Secret watch authority before any mutation: %j",async rule=>{
    const f=fixture();
    f.bindings.push({kind:"ClusterRoleBinding",metadata:{name:"watcher",uid:"binding",resourceVersion:"1"},
      roleRef:{apiGroup:"rbac.authorization.k8s.io",kind:"ClusterRole",name:"watcher"},
      subjects:[{kind:"Group",name:"system:serviceaccounts:kars-sre",apiGroup:"rbac.authorization.k8s.io"}]});
    f.objects["clusterrole//watcher"]={metadata:{name:"watcher",uid:"role",resourceVersion:"1"},rules:[rule]};
    await expect(assertSafeMutation(f.execute)).rejects.toThrow("broad group grant");
    expect(f.execute.mock.calls.every(([,args])=>args[0]==="get")).toBe(true);
  });

  it("exempts only the exact ordinary non-Secret spawner role, not a same-name watch grant",async()=>{
    const f=fixture();
    f.bindings.push({kind:"ClusterRoleBinding",metadata:{name:"spawner",uid:"binding",resourceVersion:"1"},
      roleRef:{apiGroup:"rbac.authorization.k8s.io",kind:"ClusterRole",name:"kars-sandbox-spawner"},
      subjects:[{kind:"ServiceAccount",namespace:"kars-sre",name:"sandbox"}]});
    const role={metadata:{name:"kars-sandbox-spawner",uid:"role",resourceVersion:"1"},
      rules:[{apiGroups:["kars.azure.com"],resources:["karssandboxes"],verbs:["get","list","create","delete"]}]};
    f.objects["clusterrole//kars-sandbox-spawner"]=role;
    await assertSafeMutation(f.execute);
    role.rules=[{apiGroups:[""],resources:["secrets"],verbs:["watch"]}];
    await expect(assertSafeMutation(f.execute)).rejects.toThrow("Legacy SRE grants");
    expect(f.execute.mock.calls.every(([,args])=>args[0]==="get")).toBe(true);
  });
});
