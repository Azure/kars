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
  it("keeps first-install grants and provenance immutable through retirement", () => {
    const output = execFileSync("helm", ["template", "kars", chart, "--namespace", "kars-system",
      "--show-only", "templates/sre-authority-admission.yaml", "--show-only", "templates/crd-karssreregistration.yaml"],
      { encoding: "utf8", timeout: 10000 });
    const docs = parseAllDocuments(output).map(document => document.toJSON()).filter(Boolean);
    const crd = docs.find(document => document.kind === "CustomResourceDefinition");
    const funding = crd.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.firstInstallFunding;
    expect(funding.required).toEqual(["intent", "grant"]);
    expect(funding.properties.intent.required).toEqual(["name", "uid", "digest"]);
    expect(funding.properties.grant.properties.limits.properties.tokens.maximum).toBe(9007199254740991);
    const registration = docs.find(document => document.kind === "ValidatingAdmissionPolicy" && document.metadata.name === "kars-sre-registration-authority");
    const expressions = registration.spec.validations.map((validation: any) => validation.expression);
    expect(expressions.some((expression: string) => expression.includes("object.spec.firstInstallFunding == oldObject.spec.firstInstallFunding"))).toBe(true);
    expect(expressions.some((expression: string) => expression.includes("!has(oldObject.spec.firstInstallFunding)") && expression.includes("object.spec.runtimeNamespace == oldObject.spec.runtimeNamespace"))).toBe(true);
    const source = docs.find(document => document.kind === "ValidatingAdmissionPolicy" && document.metadata.name === "kars-sre-source-authority");
    expect(source.spec.validations.some((validation: any) => validation.expression.includes("kars.azure.com/sre-first-install-intent"))).toBe(true);
  });

  it("keeps workload authorization equivalent with at most two checks per CEL expression",()=>{
    const output=execFileSync("helm",["template","kars",chart,"--namespace","kars-system",
      "--show-only","templates/sre-authority-consumers.yaml"],{encoding:"utf8",timeout:10_000});
    const policies=parseAllDocuments(output).map(doc=>doc.toJSON()).filter(doc=>doc?.kind==="ValidatingAdmissionPolicy");
    const workloads=policies.find(doc=>doc.metadata.name==="kars-sre-private-workloads");
    const registrar="authorizer.group('kars.azure.com').resource('karssreregistrations').name('canonical').check('use').allowed()";
    expect(workloads.spec.matchConditions.find((condition:{name:string})=>condition.name==="non-registrar").expression)
      .toBe(`request.namespace != 'kars-sre' || !${registrar}`);
    expect(workloads.spec.validations[0].expression.replace(/\s+/g," ")).toBe(
      "!variables.privateMaterial || authorizer.group('').resource('pods').check('create').allowed()"
      +" || (request.resource.group == 'apps' && request.resource.resource == 'replicasets' &&"
      +" authorizer.group('apps').resource('replicasets').check('create').allowed())");
    for(const policy of policies){
      expect(policy.spec.failurePolicy).toBe("Fail");
      for(const condition of [...policy.spec.matchConditions,...policy.spec.validations]){
        expect((condition.expression.match(/\.check\(/g)??[]).length).toBeLessThanOrEqual(2);
      }
    }
    for(const privateMaterial of [false,true])for(const use of [false,true])
      for(const pods of [false,true])for(const replicaSet of [false,true])for(const replicaSets of [false,true]){
        const original=!privateMaterial||use||pods||(replicaSet&&replicaSets);
        const matches=!use;
        const validation=!privateMaterial||pods||(replicaSet&&replicaSets);
        expect(!matches||validation).toBe(original);
      }
    const cronjobs=policies.find(doc=>doc.metadata.name==="kars-sre-private-cronjobs");
    expect(cronjobs.spec.validations[0].expression).toContain(registrar);
    expect(cronjobs.spec.validations[0].expression).not.toContain("replicasets");
  },30_000);

  it("escapes custom namespace fields in CEL without renaming the wire fields or weakening equality",()=>{
    const output=execFileSync("helm",["template","kars",chart,"--namespace","kars-system","--show-only","templates/crd-karssreregistration.yaml"],{encoding:"utf8",timeout:10_000});
    const registration=parseAllDocuments(output).map(doc=>doc.toJSON()).find(Boolean);
    const schema=registration.spec.versions[0].schema.openAPIV3Schema;
    const spec=schema.properties.spec;
    expect(spec.properties.sandbox.properties.namespace.type).toBe("string");
    expect(spec.properties.controller.properties.namespace.properties.name.type).toBe("string");
    expect(spec["x-kubernetes-validations"]).toEqual([{
      rule:"self.sandbox.__namespace__ == self.controller.__namespace__.name",
      message:"SRE must be registered in its controller/release namespace",
    }]);
    expect(schema["x-kubernetes-validations"][0].rule).toBe("self.metadata.name == 'canonical'");
  },30_000);

  it("creates a cluster registration and no default registrar or runtime privilege bindings",()=>{
    for(const enabled of [false,true]){
      const output=execFileSync("helm",["template","kars",chart,"--namespace","kars-system","--set",`sre.enabled=${enabled}`,
        "--set","sre.provider=azure-openai","--set","sre.model=gpt-4o-mini"],{encoding:"utf8",timeout:10_000});
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
      const privileged=controller.rules.filter((rule:any)=>
        rule.verbs.some((verb:string)=>["bind","escalate","*"].includes(verb)));
      expect(privileged).toEqual([{
        apiGroups:["rbac.authorization.k8s.io"],resources:["clusterroles"],
        resourceNames:["kars-sre-private-diagnostics","kars-sre-action-author","kars-sre-router-renew","kars-sre-reader"],
        verbs:["bind"],
      }]);
    }
  },30_000);

  it("uses authorizer permissions rather than usernames for protected source/identity admission",()=>{
    const output=execFileSync("helm",["template","kars",chart,"--namespace","kars-system"],{encoding:"utf8",timeout:10_000});
    const policies=parseAllDocuments(output).map(doc=>doc.toJSON()).filter(doc=>doc?.kind==="ValidatingAdmissionPolicy"&&doc.metadata.name.startsWith("kars-sre-"));
    expect(policies.length).toBeGreaterThan(5);
    for(const name of ["kars-sre-source-authority","kars-sre-private-identity","kars-sre-registration-authority"]){
      const policy=policies.find(doc=>doc.metadata.name===name);
      expect(policy.spec.failurePolicy).toBe("Fail");
      const expressions=JSON.stringify(policy.spec);
      expect(expressions).toContain("authorizer.group('kars.azure.com')");
      expect(expressions).not.toContain("request.userInfo.username");
    }
  },30_000);

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
    const output=execFileSync("helm",["template","kars",chart,"--namespace","kars-system"],{encoding:"utf8",timeout:10_000});
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
  },30_000);
});
