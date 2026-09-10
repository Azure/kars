// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { agentCredentialKey, validateGrantDocument } from "./credential-grants.js";
import { createHash } from "node:crypto";
import { bundleDefinition, previewPrivateActivation } from "../lib/private-activation.js";

async function fixture() {
  const objects:Record<string,any>={
    "namespace//work":{metadata:{name:"work",uid:"work-uid",resourceVersion:"1"}},
    "namespace//bridge":{metadata:{name:"bridge",uid:"bridge-uid",resourceVersion:"1"}},
    "namespace//core":{metadata:{name:"core",uid:"core-uid",resourceVersion:"1"}},
    "serviceaccount/core/kars-controller":{metadata:{name:"kars-controller",namespace:"core",uid:"controller-sa",resourceVersion:"1"}},
    "deployment/core/kars-controller":{kind:"Deployment",metadata:{name:"kars-controller",namespace:"core",uid:"controller",resourceVersion:"1"},
      spec:{template:{metadata:{},spec:{serviceAccountName:"kars-controller",containers:[{name:"controller",image:"fixture"}]}}}},
    "serviceaccount/bridge/bff":{metadata:{name:"bff",namespace:"bridge",uid:"writer-uid",resourceVersion:"1"}},
    "secret/work/kars-inference-providers":{type:"Opaque",metadata:{name:"kars-inference-providers",namespace:"work",uid:"store-uid",resourceVersion:"2"},
      data:{COPILOT_GITHUB_TOKEN:"PRIVATE_VALUE_SENTINEL"}},
  };
  objects["deployments.apps/core/kars-controller"]=objects["deployment/core/kars-controller"];
  for(const [index,definition] of (bundleDefinition().objects as any[]).entries()){
    const object=structuredClone(definition);
    object.metadata={...object.metadata,uid:`policy-${index}`,resourceVersion:"1",generation:1};
    if(object.kind==="ValidatingAdmissionPolicy")object.status={observedGeneration:1,typeChecking:{}};
    objects[`${object.kind.toLowerCase()}//${object.metadata.name}`]=object;
  }
  const execute=vi.fn(async(args:string[])=>{
    if(args[0]==="auth")return "yes";
    const namespace=args.includes("-n")?args[args.indexOf("-n")+1]:"";
    return JSON.stringify(objects[`${args[1]}/${namespace}/${args[2]}`]??null);
  });
  const document={apiVersion:"kars.azure.com/v1alpha1",kind:"KarsCredentialGrant",
    metadata:{name:"workspace",namespace:"work"},
    spec:{workspaceUid:"work-uid",writers:[{namespace:"bridge",name:"bff",uid:"writer-uid"}],
      agentKeys:["GITHUB_TOKEN"],integrationStores:[{secret:{name:"kars-inference-providers",uid:"store-uid"},purpose:"providers"}],
      legacyImports:[],enabled:true}};
  const privateActivation=await previewPrivateActivation(execute,"work",document.spec.writers,[],"core","kcm-certificate",[]);
  execute.mockClear();
  return {objects,execute,document:{...document,spec:{...document.spec,privateActivation}}};
}

describe("operator credential grant preflight",()=>{
  it("accepts reviewed identities without mutation or echoing credential values",async()=>{
    const f=await fixture();
    await validateGrantDocument(f.execute,f.document);
    expect(f.execute.mock.calls.every(([args])=>["get","auth"].includes(args[0]!))).toBe(true);
    expect(JSON.stringify(f.document)).not.toContain("PRIVATE_VALUE_SENTINEL");
  });
  it("allows explicit writer retirement without disabling existing delivery authority",async()=>{
    const f=await fixture();
    f.document.spec.writers=[];
    await validateGrantDocument(f.execute,f.document);
    expect(f.document.spec.enabled).toBe(true);
    expect(f.execute.mock.calls.some(([args])=>args[1]==="serviceaccount")).toBe(false);
    expect(f.execute.mock.calls.every(([args])=>["get","auth"].includes(args[0]!))).toBe(true);
  });
  it.each(["workspace","writer","store"])("rejects replaced %s identities before any mutation",async changed=>{
    const f=await fixture();
    if(changed==="workspace")f.document.spec.workspaceUid="other";
    if(changed==="writer")f.document.spec.writers[0]!.uid="other";
    if(changed==="store")f.document.spec.integrationStores[0]!.secret.uid="other";
    await expect(validateGrantDocument(f.execute,f.document)).rejects.toThrow(/UID.*changed/);
    expect(f.execute.mock.calls.every(([args])=>["get","auth"].includes(args[0]!))).toBe(true);
  });
  it("rejects grants without operator permission",async()=>{
    const f=await fixture();
    f.execute.mockResolvedValue("no");
    await expect(validateGrantDocument(f.execute,f.document)).rejects.toThrow("operator permission");
    expect(f.execute).toHaveBeenCalledTimes(1);
  });
  it("rejects raw credential fields and bootstrap-variable grants",async()=>{
    const f=await fixture();
    await expect(validateGrantDocument(f.execute,{...f.document,spec:{...f.document.spec,data:{TOKEN:"secret"}}}))
      .rejects.toThrow("metadata-only");
    for(const key of ["NODE_OPTIONS","PATH","LD_PRELOAD","AZURE_CLIENT_SECRET","KARS_ADMIN_TOKEN","OPENAI_API_KEY","JAVA_TOOL_OPTIONS"]){
      expect(agentCredentialKey(key),key).toBe(false);
    }
    expect(agentCredentialKey("GITHUB_TOKEN")).toBe(true);
    expect(agentCredentialKey("INTERNAL_SERVICE_SECRET")).toBe(true);
  });
  it("preflights immutable GitHub source identities and canonical reviewed scope without writes",async()=>{
    const f=await fixture();
    const name=`kars-github-connection-${createHash("sha256").update("owner").digest("hex").slice(0,16)}`;
    f.objects[`configmap/work/${name}`]={metadata:{name,uid:"connection",resourceVersion:"1"},
      data:{installation_id:"456",repos:'["owner/repo"]'}};
    f.objects["secret/work/kars-github-app"]={type:"Opaque",metadata:{name:"kars-github-app",uid:"app",resourceVersion:"1"},
      data:{GITHUB_APP_ID:Buffer.from("123").toString("base64"),GITHUB_APP_PRIVATE_KEY:"PRIVATE_VALUE_SENTINEL"}};
    f.document.spec.integrationStores.push({secret:{name:"kars-github-app",uid:"app"},purpose:"github-app"});
    const connection={connection:{name,uid:"connection"},appSecret:{name:"kars-github-app",uid:"app"},
      appId:"123",ownerSubject:"owner",installationId:456,repositories:["owner/repo"],write:false};
    const document={...f.document,spec:{...f.document.spec,githubConnections:[connection]}};
    await validateGrantDocument(f.execute,document);
    connection.connection.uid="replacement";
    await expect(validateGrantDocument(f.execute,document)).rejects.toThrow("review changed");
    expect(f.execute.mock.calls.every(([args])=>["get","auth"].includes(args[0]!))).toBe(true);
    expect(JSON.stringify(document)).not.toContain("PRIVATE_VALUE_SENTINEL");
  });
});
