import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
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

async function legacyRender(args:string[],lookup=false):Promise<{objects:any[];calls:string[]}> {
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
    const groups=["apps","rbac.authorization.k8s.io","networking.k8s.io"];
    const discovery:Record<string,Array<[string,string,boolean]>>={
      "/api/v1":[["namespaces","Namespace",false],["services","Service",true],["serviceaccounts","ServiceAccount",true],
        ["configmaps","ConfigMap",true],["secrets","Secret",true]],
      "/apis/apps/v1":[["deployments","Deployment",true]],
      "/apis/rbac.authorization.k8s.io/v1":[["roles","Role",true],["rolebindings","RoleBinding",true],
        ["clusterroles","ClusterRole",false],["clusterrolebindings","ClusterRoleBinding",false]],
      "/apis/networking.k8s.io/v1":[["networkpolicies","NetworkPolicy",true],["ingresses","Ingress",true]],
    };
    const value=path==="/version"?{major:"1",minor:"32",gitVersion:"v1.32.0"}:
      path==="/api"?{apiVersion:"v1",kind:"APIVersions",versions:["v1"],serverAddressByClientCIDRs:[]}:
      path==="/apis"?{apiVersion:"v1",kind:"APIGroupList",groups:groups.map(name=>({name,
        versions:[{groupVersion:`${name}/v1`,version:"v1"}],preferredVersion:{groupVersion:`${name}/v1`,version:"v1"}}))}:
      discovery[path]?{apiVersion:"v1",kind:"APIResourceList",groupVersion:path==="/api/v1"?"v1":path.slice("/apis/".length),
        resources:resources(discovery[path]!)}:
      path==="/api/v1/namespaces/kars-system"?{apiVersion:"v1",kind:"Namespace",metadata:{name:"kars-system",uid:"existing",
        labels:{customer:"retained"},annotations:{"meta.helm.sh/release-name":"kars-bridge",
          "meta.helm.sh/release-namespace":"kars-system",customer:"retained"}}}:null;
    response.writeHead(value?200:404,{"content-type":"application/json"});
    response.end(JSON.stringify(value??{apiVersion:"v1",kind:"Status",code:404,reason:"NotFound"}));
  });
  try {
    const extra:string[]=[];
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
    expect(values.networkPolicy.observations).toBeUndefined();
  });
  it.each([[],["--set","core.namespace="]].map(args=>({args})))("defaults BFF/web workspace and observations off with old values $args",async({args})=>{
    const {objects}=await legacyRender(args);
    expect(objects.some(item=>item.metadata?.name==="kars-bridge-observation-egress")).toBe(false);
    for(const name of ["kars-bridge-bff","kars-bridge-web"]){
      const env=objects.find(item=>item.kind==="Deployment"&&item.metadata.name===name).spec.template.spec.containers[0].env;
      expect(env.find((item:any)=>item.name==="BRIDGE_DEFAULT_NAMESPACE").value).toBe("kars-system");
      if(name.endsWith("bff"))expect(env.find((item:any)=>item.name==="BRIDGE_CORE_NAMESPACE").value).toBe("kars-system");
    }
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
});
