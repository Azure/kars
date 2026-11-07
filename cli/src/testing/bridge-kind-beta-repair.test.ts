// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const templates = fileURLToPath(new URL("../../../bridge/deploy/helm/kars-bridge/templates/", import.meta.url));
function fixture(): any {
  const owner = { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" };
  const intent = JSON.stringify({ funding: { tokens: 1000, expiresAt: "2020-01-01T00:00:00Z" } });
  return {
    controller: { metadata: { uid: "controller-uid" } },
    registration: { metadata: { generation: 1 }, spec: { enabled: true, controller: { deployment: { uid: "controller-uid" } }, firstInstallFunding: { intent: { uid: "intent-uid", digest: createHash("sha256").update(intent).digest("hex") } } },
      status: { phase: "Blocked", observedGeneration: 1, conditions: [{ type: "Ready", status: "False", reason: "AuthorityBlocked" }] } },
    bff: { metadata: { uid: "bff-uid", annotations: { ...owner, "meta.helm.sh/release-name": "kars-bridge" } } },
    intent: { immutable: true, metadata: { uid: "intent-uid", annotations: owner }, data: { "intent.json": intent } },
    adoption: { immutable: true, metadata: { uid: "adoption-uid", annotations: owner }, data: { intentUID: "intent-uid", valuesDigest: "a".repeat(64) } },
  };
}
function render(input: any, upgrade = true, section: "registration" | "admission" | "oidc" = "registration") {
  const dir = mkdtempSync(join(tmpdir(), "kars-bridge-repair-"));
  try {
    mkdirSync(join(dir, "templates"));
    writeFileSync(join(dir, "Chart.yaml"), "apiVersion: v2\nname: repair-test\nversion: 0.1.0\n");
    writeFileSync(join(dir, "values.yaml"), JSON.stringify(section === "registration" ? { fixture: input } : input));
    copyFileSync(join(templates, "_sre-repair.tpl"), join(dir, "templates/_sre-repair.tpl"));
    const profile = readFileSync(join(templates, "kind-beta.yaml"), "utf8");
    const boundaries = {
      admission: ['{{- if or (not .Values.networkPolicy.create)', '{{- $images :='],
      registration: ['{{- $registration := lookup', '{{- $cni :='],
      oidc: ['{{- $env := dict', '{{- $controller := lookup'],
    };
    const start = profile.indexOf(boundaries[section][0]);
    const end = profile.indexOf(boundaries[section][1], start);
    expect(start).toBeGreaterThan(0);
    expect(end).toBeGreaterThan(start);
    let gate = profile.slice(start, end);
    for (const [lookup, field] of [
      ['lookup "kars.azure.com/v1alpha1" "KarsSRERegistration" "" "canonical"', "registration"],
      ['lookup "apps/v1" "Deployment" .Release.Namespace "kars-bridge-bff"', "bff"],
      ['lookup "v1" "ConfigMap" "kars-system" "kars-sre-first-install"', "intent"],
      ['lookup "v1" "ConfigMap" "kars-system" "kars-sre-helm-adoption"', "adoption"],
    ]) {
      if (section === "registration") {
        expect(gate).toContain(lookup);
        gate = gate.replace(lookup, `.Values.fixture.${field}`);
      }
    }
    writeFileSync(join(dir, "templates/result.yaml"), (section === "registration" ? '{{- $controller := .Values.fixture.controller -}}\n' : "") + gate);
    return execFileSync("helm", ["template", "kars-bridge", dir, "-n", "kars-system", ...(upgrade ? ["--is-upgrade"] : [])],
      { encoding: "utf8", timeout: 10000, stdio: ["ignore", "pipe", "pipe"] });
  } finally { rmSync(dir, { recursive: true, force: true }); }
}
describe("Kind Bridge workflow validation configuration", () => {
  const settings = (mode: string) => ({ networkPolicy: { create: true }, bff: { routeQualification: { mode }, extraEnv: [] } });
  it.each(["required", "validation"])("permits explicit %s mode without weakening network or env controls", mode => {
    expect(render(settings(mode), true, "admission").trim()).toBe("");
  });
  it.each(["", "disabled", "Validation", "validation "])("rejects invalid mode %j", mode => {
    expect(() => render(settings(mode), true, "admission")).toThrow(/required or validation route qualification mode/);
  });
  it.each(["required", "validation"])("still rejects missing NetworkPolicy and BFF overrides in %s mode", mode => {
    const values = settings(mode);
    values.networkPolicy.create = false;
    expect(() => render(values, true, "admission")).toThrow(/enforced NetworkPolicy/);
    values.networkPolicy.create = true;
    expect(() => render({ ...values, bff: { ...values.bff, extraEnv: [{ name: "BRIDGE_ROUTE_QUALIFICATION_MODE", value: "validation" }] } }, true, "admission"))
      .toThrow(/no BFF env overrides/);
  });
});
describe("Kind Bridge Headlamp SSO configuration", () => {
  function settings(): any {
    return { auth: { principalSecretName: "principal", principalSecretKey: "session-secret" }, web: { extraEnv: [
      { name: "BRIDGE_OIDC_ISSUER", value: "https://identity.example" },
      { name: "BRIDGE_OIDC_CLIENT_ID", value: "bridge" },
      { name: "BRIDGE_OIDC_CLIENT_SECRET", valueFrom: { secretKeyRef: { name: "oidc", key: "client-secret" } } },
      { name: "BRIDGE_OIDC_REDIRECT_URI", value: "http://localhost:3181/auth/callback" },
      { name: "BRIDGE_OIDC_ROLE_CLAIM", value: "groups" },
      { name: "BRIDGE_OIDC_ROLE_MAP", value: '{"admins":"admin"}' },
    ] } };
  }
  it.each([undefined, "true", "false"])("permits the optional literal flag %j with genuine OIDC", value => {
    const values = settings();
    if (value !== undefined) values.web.extraEnv.push({ name: "BRIDGE_HEADLAMP_SSO_ENABLED", value });
    expect(render(values, true, "oidc").trim()).toBe("");
  });
  it.each([true, false, "TRUE", "1", "", null])("rejects a non-literal flag %j", value => {
    const values = settings();
    values.web.extraEnv.push({ name: "BRIDGE_HEADLAMP_SSO_ENABLED", value });
    expect(() => render(values, true, "oidc")).toThrow(/literal string true or false/);
  });
  it("rejects indirect flag values, duplicates and unrelated overrides", () => {
    for (const entry of [
      { name: "BRIDGE_HEADLAMP_SSO_ENABLED", value: "true", valueFrom: { secretKeyRef: { name: "settings", key: "flag" } } },
      { name: "BRIDGE_HEADLAMP_SSO_ENABLED", valueFrom: { configMapKeyRef: { name: "settings", key: "flag" } } },
      { name: "BRIDGE_HEADLAMP_SSO_ENABLED", value: "false" },
      { name: "BRIDGE_AUTH_MODE", value: "development" },
    ]) {
      const values = settings();
      if (!("valueFrom" in entry)) values.web.extraEnv.push({ name: "BRIDGE_HEADLAMP_SSO_ENABLED", value: "true" });
      values.web.extraEnv.push(entry);
      expect(() => render(values, true, "oidc")).toThrow(/unique genuine OIDC|literal string true or false/);
    }
  });
  it.each(["issuer", "secret", "roles"])("does not weaken the %s requirement", fault => {
    const values = settings();
    values.web.extraEnv.push({ name: "BRIDGE_HEADLAMP_SSO_ENABLED", value: "true" });
    if (fault === "issuer") values.web.extraEnv[0].value = "http://identity.example";
    if (fault === "secret") values.web.extraEnv[2] = { name: "BRIDGE_OIDC_CLIENT_SECRET", value: "inline" };
    if (fault === "roles") values.web.extraEnv.find((entry: { name: string }) => entry.name === "BRIDGE_OIDC_ROLE_MAP").value = "{}";
    expect(() => render(values, true, "oidc")).toThrow(/genuine HTTPS OIDC|Secret references|nonempty operator OIDC role map/);
  });
});
describe("Kind Bridge adopted reader repair", () => {
  it("allows existing adopted blocked repair without changing funding or status", () => {
    const f = fixture();
    const before = structuredClone(f);
    expect(render(f).trim()).toBe("");
    expect(f).toEqual(before);
  });
  it("does not allow blocked initial installation", () => {
    expect(() => render(fixture(), false)).toThrow(/no partial-install continuation/);
  });
  it("preserves Ready initial enrollment without requiring an existing Bridge", () => {
    const f = fixture();
    f.registration.status.phase = "Ready";
    f.bff = {}; f.intent = {}; f.adoption = {};
    expect(() => render(f, false)).not.toThrow();
  });
  it.each(["missing-bridge", "bridge-owner", "bridge-namespace", "deleting-bridge", "missing-intent", "missing-adoption", "mutable-intent", "mutable-adoption", "deleting-intent", "intent-owner", "adoption-owner", "intent-uid", "intent-digest", "adoption-intent", "adoption-digest", "controller-uid", "stale-generation", "disabled", "deleting-registration", "wrong-phase", "wrong-condition"])("rejects %s", fault => {
    const f = fixture();
    if (fault === "missing-bridge") f.bff = {};
    if (fault === "bridge-owner") f.bff.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "bridge-namespace") f.bff.metadata.annotations["meta.helm.sh/release-namespace"] = "foreign";
    if (fault === "deleting-bridge") f.bff.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "missing-intent") f.intent = {};
    if (fault === "missing-adoption") f.adoption = {};
    if (fault === "mutable-intent") f.intent.immutable = false;
    if (fault === "mutable-adoption") f.adoption.immutable = false;
    if (fault === "deleting-intent") f.intent.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "intent-owner") f.intent.metadata.annotations = {};
    if (fault === "adoption-owner") f.adoption.metadata.annotations = {};
    if (fault === "intent-uid") f.intent.metadata.uid = "replacement";
    if (fault === "intent-digest") f.intent.data["intent.json"] += " ";
    if (fault === "adoption-intent") f.adoption.data.intentUID = "replacement";
    if (fault === "adoption-digest") f.adoption.data.valuesDigest = "invalid";
    if (fault === "controller-uid") f.registration.spec.controller.deployment.uid = "replacement";
    if (fault === "stale-generation") f.registration.metadata.generation++;
    if (fault === "disabled") f.registration.spec.enabled = false;
    if (fault === "deleting-registration") f.registration.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "wrong-phase") f.registration.status.phase = "Pending";
    if (fault === "wrong-condition") f.registration.status.conditions[0].reason = "Pending";
    expect(() => render(f)).toThrow(/no partial-install continuation/);
  });
});
