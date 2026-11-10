// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
import { execFileSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { copyFileSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parse } from "yaml";

const root = fileURLToPath(new URL("../../../", import.meta.url));
function fixture(): any {
  const owner = { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" };
  const encoded = JSON.stringify({ funding: { unboundedTokens: true } });
  const digest = createHash("sha256").update(encoded).digest("hex");
  const grant = { root: { kind: "KarsSandbox", resource: { name: "sre", namespace: "kars-system", uid: "sre-uid" },
    workspaceUid: "workspace-uid", clusterUid: "cluster-uid" }, limits: {}, unboundedTokens: true };
  const receipt = { id: `first-install-${digest}`, configUid: "catalog-uid", accountUid: "account-uid",
    root: grant.root, originalLimits: {}, ledgerDigest: `sha256:${"a".repeat(64)}`, previousTokenTopUpIds: [] };
  return { desired: [], release: "kars", namespace: "kars-system",
    intent: { immutable: true, metadata: { uid: "intent-uid", annotations: owner }, data: { "intent.json": encoded } },
    catalog: { metadata: { uid: "catalog-uid", annotations: owner }, data: { "unbounded-token-allowances.json": JSON.stringify([receipt]) } },
    registration: { metadata: {}, spec: { sandbox: { uid: "sre-uid" }, firstInstallFunding: { intent: { uid: "intent-uid", digest }, grant } } },
    source: { metadata: { uid: "sre-uid" }, spec: { inferenceBudgetRef: { root: grant.root, account: { uid: "account-uid" } } } },
    account: { metadata: { uid: "account-uid" }, spec: { root: grant.root, limits: {} }, status: { ledger: { root: grant.root, unboundedTokens: receipt } } } };
}
function render(input: any, raw = false) {
  const dir = `${root}.sre-allowance-check-${randomUUID()}`;
  mkdirSync(`${dir}/templates`, { recursive: true });
  try {
    writeFileSync(`${dir}/Chart.yaml`, "apiVersion: v2\nname: allowance-check\nversion: 0.1.0\n");
    writeFileSync(`${dir}/fixture.json`, JSON.stringify(input));
    copyFileSync(`${root}deploy/helm/kars/templates/_sre-first-install-allowance.tpl`, `${dir}/templates/_allowance.tpl`);
    writeFileSync(`${dir}/templates/result.yaml`, '{{ $input := .Files.Get "fixture.json" | fromJson }}\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: result\ndata:\n  receipts: {{ include "kars.sre.firstInstallAllowances" $input | quote }}\n');
    const text = execFileSync("helm", ["template", "allowance-check", dir], { encoding: "utf8", timeout: 10000, stdio: ["ignore", "pipe", "pipe"] });
    const receipts = parse(text).data.receipts;
    return raw ? receipts : JSON.parse(receipts);
  } finally { rmSync(dir, { recursive: true, force: true }); }
}

describe("cold-install allowance Helm continuity", () => {
  it("preserves exactly the live consumed receipt without changing operator values", () => {
    const f = fixture();
    const before = structuredClone(f);
    expect(render(f)).toEqual([f.account.status.ledger.unboundedTokens]);
    expect(f).toEqual(before);
    f.desired = [f.account.status.ledger.unboundedTokens];
    expect(render(f)).toEqual(f.desired);
  });
  it("preserves live JSON bytes after validating semantically identical receipts", () => {
    const f = fixture();
    const stored = JSON.stringify([f.account.status.ledger.unboundedTokens], null, 2);
    f.catalog.data["unbounded-token-allowances.json"] = stored;
    expect(render(f, true)).toBe(stored);
    f.desired = [f.account.status.ledger.unboundedTokens];
    expect(render(f, true)).toBe(stored);
  });
  it("renders changed desired allowances rather than retaining stale catalog bytes", () => {
    const f = fixture();
    const receipt = f.account.status.ledger.unboundedTokens;
    const other = { ...receipt, id: "operator-authorized", accountUid: "other-account",
      root: { ...receipt.root, resource: { ...receipt.root.resource, name: "other" } } };
    f.desired = [other];
    expect(render(f)).toEqual([other, receipt]);
    expect(render(f, true)).not.toBe(f.catalog.data["unbounded-token-allowances.json"]);
  });
  it("does not restore a revoked receipt", () => {
    const f = fixture(); f.catalog.data["unbounded-token-allowances.json"] = "[]";
    expect(render(f)).toEqual([]);
  });
  it.each(["catalog", "account", "source", "intent", "digest", "root", "grant", "expiry", "consumed", "configured", "owner", "deleted"])("rejects changed %s authority", fault => {
    const f = fixture();
    if (["catalog", "account", "source", "intent"].includes(fault)) f[fault].metadata.uid = "replaced";
    if (fault === "digest") f.registration.spec.firstInstallFunding.intent.digest = "b".repeat(64);
    if (fault === "root") f.account.spec.root = { ...f.account.spec.root, workspaceUid: "replaced" };
    if (fault === "grant") f.registration.spec.firstInstallFunding.grant.unboundedTokens = false;
    if (fault === "expiry") f.registration.spec.firstInstallFunding.grant.expiresAt = "2999-01-01T00:00:00Z";
    if (fault === "consumed") f.account.status.ledger.unboundedTokens = { ...f.account.status.ledger.unboundedTokens, id: "replaced" };
    if (fault === "configured") f.desired = [{ ...f.account.status.ledger.unboundedTokens, ledgerDigest: `sha256:${"b".repeat(64)}` }];
    if (fault === "owner") f.catalog.metadata.annotations = {};
    if (fault === "deleted") f.source.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    expect(() => render(f)).toThrow();
  });
});
