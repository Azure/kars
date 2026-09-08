// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";

const root = new URL("../../../", import.meta.url);
const source = (path: string) => readFileSync(new URL(path, root), "utf8");

describe("namespace ownership wiring", () => {
  it("checks ownership before either upgrade path invokes Helm", () => {
    const upgrade = source("cli/src/commands/upgrade.ts");
    expect(upgrade.indexOf("await inspectNamespaceOwnership(execa)"))
      .toBeLessThan(upgrade.indexOf('stepper.step(`Importing ${target}'));
    const fast = source("cli/src/commands/up/fast_upgrade.ts");
    expect(fast.indexOf("await inspectNamespaceOwnership(execa)"))
      .toBeLessThan(fast.indexOf('const helmArgs = ['));
  });

  it("claims before controller side effects and retains namespace cleanup ordering", () => {
    const reconciler = source("controller/src/reconciler/mod.rs");
    expect(reconciler.indexOf("namespace_ownership::ensure"))
      .toBeLessThan(reconciler.indexOf("if sandbox.metadata.deletion_timestamp.is_some()"));
    expect(reconciler.indexOf("namespace_ownership::delete"))
      .toBeLessThan(reconciler.indexOf("namespace_ownership::remove_finalizer"));
    expect(reconciler).not.toContain("ns_api.delete");
    expect(source("controller/src/reconciler/namespace_ownership.rs")).not.toContain(".force()");
    expect(reconciler.split("\n").length - 1).toBeLessThanOrEqual(3700);
  });

  it("registers generic diagnostics with the existing kube context safeguard", () => {
    expect(source("cli/src/cli.ts")).toContain("program.addCommand(namespaceCommand())");
    expect(source("cli/src/lib/kube-bootstrap.ts")).toContain('"namespace"');
    const add = source("cli/src/commands/add.ts");
    expect(add.indexOf("await prepareCredentialNamespace"))
      .toBeLessThan(add.indexOf('const secretArgs = ["create", "secret"'));
    expect(add).toContain("[CLAIM.namespaceUid]: namespaceUid");
  });

  it("guards approval cleanup and admin-token reads with workspace-aware ownership", () => {
    const approvals = source("controller/src/egress_approval_reconciler.rs");
    expect(approvals.indexOf("namespace_ownership::verify_target"))
      .toBeLessThan(approvals.indexOf("return finalize(&api"));
    const confirmation = source("controller/src/status/router_confirmation_io.rs");
    expect(confirmation.indexOf("namespace_ownership::verify_target"))
      .toBeLessThan(confirmation.indexOf('api.get_opt("router-admin-token")'));
    expect(confirmation).toContain("read_admin_token(client, source_namespace, sandbox)");
    expect(confirmation).toContain("namespace_ownership::lock(sandbox)");
    expect(approvals).toContain("drop(namespace_lock)");
    expect(approvals).toContain('"uid": approval.metadata.uid');
    expect(approvals).toContain('"resourceVersion": approval.metadata.resource_version');
  });

  it("grants the controller the namespace watch needed by the new secondary watch", () => {
    const output = execFileSync("helm", [
      "template", "kars", fileURLToPath(new URL("deploy/helm/kars", root)),
      "--namespace", "kars-system", "--show-only", "templates/rbac.yaml",
    ], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 30_000 });
    const docs = parseAllDocuments(output).map(document => {
      if (document.errors.length) throw document.errors[0];
      return document.toJSON();
    });
    const controller = docs.find(doc => doc?.kind === "ClusterRole" && doc.metadata?.name === "kars-controller");
    expect(controller).toBeDefined();
    const rule = controller.rules.find((rule: { resources: string[] }) => rule.resources.includes("namespaces"));
    expect(rule.verbs).toContain("watch");
  });
});
