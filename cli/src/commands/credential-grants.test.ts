// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { agentCredentialKey, validateGrantDocument } from "./credential-grants.js";

function fixture() {
  const objects:Record<string,any>={
    "namespace//work":{metadata:{name:"work",uid:"work-uid",resourceVersion:"1"}},
    "serviceaccount/bridge/bff":{metadata:{name:"bff",namespace:"bridge",uid:"writer-uid",resourceVersion:"1"}},
    "secret/work/kars-inference-providers":{type:"Opaque",metadata:{name:"kars-inference-providers",namespace:"work",uid:"store-uid",resourceVersion:"2"},
      data:{COPILOT_GITHUB_TOKEN:"PRIVATE_VALUE_SENTINEL"}},
  };
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
  return {objects,execute,document};
}

describe("operator credential grant preflight",()=>{
  it("accepts reviewed identities without mutation or echoing credential values",async()=>{
    const f=fixture();
    await validateGrantDocument(f.execute,f.document);
    expect(f.execute.mock.calls.every(([args])=>["get","auth"].includes(args[0]!))).toBe(true);
    expect(JSON.stringify(f.document)).not.toContain("PRIVATE_VALUE_SENTINEL");
  });
  it.each(["workspace","writer","store"])("rejects replaced %s identities before any mutation",async changed=>{
    const f=fixture();
    if(changed==="workspace")f.document.spec.workspaceUid="other";
    if(changed==="writer")f.document.spec.writers[0]!.uid="other";
    if(changed==="store")f.document.spec.integrationStores[0]!.secret.uid="other";
    await expect(validateGrantDocument(f.execute,f.document)).rejects.toThrow(/UID.*changed/);
    expect(f.execute.mock.calls.every(([args])=>["get","auth"].includes(args[0]!))).toBe(true);
  });
  it("rejects grants without operator permission",async()=>{
    const f=fixture();
    f.execute.mockResolvedValue("no");
    await expect(validateGrantDocument(f.execute,f.document)).rejects.toThrow("operator permission");
    expect(f.execute).toHaveBeenCalledTimes(1);
  });
  it("rejects raw credential fields and bootstrap-variable grants",async()=>{
    const f=fixture();
    await expect(validateGrantDocument(f.execute,{...f.document,spec:{...f.document.spec,data:{TOKEN:"secret"}}}))
      .rejects.toThrow("metadata-only");
    for(const key of ["NODE_OPTIONS","PATH","LD_PRELOAD","AZURE_CLIENT_SECRET","KARS_ADMIN_TOKEN","OPENAI_API_KEY","JAVA_TOOL_OPTIONS"]){
      expect(agentCredentialKey(key),key).toBe(false);
    }
    expect(agentCredentialKey("GITHUB_TOKEN")).toBe(true);
    expect(agentCredentialKey("INTERNAL_SERVICE_SECRET")).toBe(true);
  });
});
