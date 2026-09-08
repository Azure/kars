// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe,expect,it } from "vitest";
import { parseAllDocuments } from "yaml";

const root=fileURLToPath(new URL("../../../",import.meta.url));
const chart=fileURLToPath(new URL("../../../deploy/helm/kars",import.meta.url));

describe("SRE authority chart and mutation integration",()=>{
  it("creates a cluster registration and no default registrar or runtime privilege bindings",()=>{
    for(const enabled of [false,true]){
      const output=execFileSync("helm",["template","kars",chart,"--namespace","kars-system","--set",`sre.enabled=${enabled}`],{encoding:"utf8"});
      const docs=parseAllDocuments(output).map(doc=>doc.toJSON()).filter(Boolean);
      const registration=docs.find(doc=>doc.kind==="CustomResourceDefinition"&&doc.metadata.name==="karssreregistrations.kars.azure.com");
      expect(registration.spec.scope).toBe("Cluster");
      const bindings=docs.filter(doc=>["ClusterRoleBinding","RoleBinding"].includes(doc.kind));
      expect(bindings.some(binding=>binding.roleRef.name==="kars-sre-registrar")).toBe(false);
      expect(bindings.some(binding=>(binding.subjects??[]).some((subject:any)=>
        subject.namespace==="kars-sre"&&["sandbox","sre-api-router"].includes(subject.name)))).toBe(false);
      const controller=docs.find(doc=>doc.kind==="ClusterRole"&&doc.metadata.name==="kars-sre-authority-controller");
      expect(controller.rules.filter((rule:any)=>rule.resources.includes("karssreregistrations"))
        .every((rule:any)=>rule.verbs.every((verb:string)=>["get","list","watch","use"].includes(verb)))).toBe(true);
    }
  });

  it("uses authorizer permissions rather than usernames for protected source/identity admission",()=>{
    const output=execFileSync("helm",["template","kars",chart,"--namespace","custom-controller"],{encoding:"utf8"});
    const policies=parseAllDocuments(output).map(doc=>doc.toJSON()).filter(doc=>doc?.kind==="ValidatingAdmissionPolicy"&&doc.metadata.name.startsWith("kars-sre-"));
    expect(policies.length).toBeGreaterThan(5);
    for(const name of ["kars-sre-source-authority","kars-sre-private-identity","kars-sre-registration-authority"]){
      const policy=policies.find(doc=>doc.metadata.name===name);
      expect(policy.spec.failurePolicy).toBe("Fail");
      const expressions=JSON.stringify(policy.spec);
      expect(expressions).toContain("authorizer.group('kars.azure.com')");
      expect(expressions).not.toContain("request.userInfo.username");
    }
  });

  it("places normal mutation and rollback checks before their owning writes",()=>{
    const checks=[
      ["cli/src/commands/push-apply.ts","await assertSafeMutation(execute)","const artifacts ="],
      ["cli/src/commands/up/fast_upgrade.ts","await assertSafeMutation(execa)","const helmArgs ="],
      ["cli/src/commands/up.ts","await assertSafeMutation(execa)","const helmArgs ="],
      ["cli/src/commands/dev/local-k8s.ts","await assertSafeMutation(","const credsOverlay = await provisionDevCreds"],
      ["cli/src/commands/upgrade.ts","await assertRollbackSafe(execa)","await execa(\"helm\", [\"rollback\""],
    ];
    for(const [path,gate,write] of checks){
      const source=readFileSync(`${root}/${path}`,"utf8");
      expect(source.indexOf(gate),path).toBeGreaterThanOrEqual(0);
      expect(source.indexOf(gate),path).toBeLessThan(source.indexOf(write));
    }
  });

  it("unconditionally denies legacy token Secret creation and both old/new update transitions under arbitrary names",()=>{
    const output=execFileSync("helm",["template","kars",chart],{encoding:"utf8"});
    const docs=parseAllDocuments(output).map(doc=>doc.toJSON()).filter(Boolean);
    const policy=docs.find(doc=>doc.kind==="ValidatingAdmissionPolicy"&&doc.metadata.name==="kars-sre-no-legacy-tokens");
    expect(policy.spec.matchConstraints.resourceRules).toEqual([{
      apiGroups:[""],apiVersions:["v1"],operations:["CREATE","UPDATE"],resources:["secrets"],
    }]);
    const expression=policy.spec.validations[0].expression;
    expect(expression).toContain("oldObject");
    expect(expression).toContain("object.?type");
    expect(expression).toContain("kubernetes.io/service-account.name");
    expect(expression).toContain("kubernetes.io/service-account-token");
    expect(expression).not.toContain("metadata.name");
    expect(expression).not.toContain("authorizer");
    expect(docs.find(doc=>doc.kind==="ValidatingAdmissionPolicyBinding"
      &&doc.metadata.name===policy.metadata.name).spec.validationActions).toContain("Deny");
  });
});
