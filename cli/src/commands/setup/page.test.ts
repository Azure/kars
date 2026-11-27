// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Script } from "node:vm";
import { describe, expect, it } from "vitest";
import { renderSetupPage } from "./page.js";

const page = renderSetupPage();
const scripts = [...page.matchAll(/<script>([\s\S]*?)<\/script>/gi)].map(match => match[1]);

describe("browser setup document", () => {
  it("is self-contained, deterministic, and has executable JavaScript", () => {
    expect(renderSetupPage()).toBe(page);
    expect(scripts).toHaveLength(1);
    expect(() => new Script(scripts[0])).not.toThrow();
    expect(page).not.toMatch(/<(script|link|img|iframe)[^>]+(?:src|href)=["']https?:/);
    expect(page).not.toMatch(/\son(?:click|load|input|submit)=/);
    expect(page).toContain('<meta name="referrer" content="no-referrer">');
  });

  it("emits valid Unicode-set HTML input patterns", () => {
    const patterns = [...page.matchAll(/\bpattern="([^"]+)"/g)].map(match => match[1]);
    expect(patterns.length).toBeGreaterThan(0);
    for (const pattern of patterns) expect(() => new RegExp(pattern, "v")).not.toThrow();
    const modelPattern = page.match(/id="model"[^>]*pattern="([^"]+)"/)?.[1];
    expect(modelPattern).toBeDefined();
    const model = new RegExp(`^(?:${modelPattern})$`, "v");
    expect(model.test("org/model-1.2:latest")).toBe(true);
    expect(model.test("model with spaces")).toBe(false);
  });

  it("offers broad providers without advertising unimplemented local provisioning", () => {
    for (const kind of ['github-copilot', 'openai', 'anthropic', 'foundry', 'azure-openai', 'openai-compatible']) expect(page).toContain(`value="${kind}"`);
    expect(page).toContain('/api/copilot/start'); expect(page).toContain('/api/copilot/cancel');
    expect(page).toContain('https://github.com/login/device');
    expect(page).toContain('Native Messages-only features are not enabled');
    expect(page).toContain('does not deploy a model or GPU');
    expect(page).toContain("$('api-key').value = ''; $('model').value = ''");
  });
  it("offers Core-only, existing-context, provider and first-administrator flows", () => {
    expect(page).toContain('name="target" value="kind"');
    expect(page).toContain('name="target" value="existing"');
    expect(page).toContain('id="bridge" type="checkbox" checked');
    expect(page).toContain('id="admin-password" type="password"');
    expect(page).toContain('id="admin-confirm" type="password"');
    expect(page).toContain('id="scope-note"');
    expect(page).toContain('inspection-only');
    expect(page).toContain('id="trust" type="checkbox"');
    expect(page).toContain('id="approve" type="checkbox"');
    expect(page).toContain('id="install" class="primary" disabled');
    expect(page).not.toContain('type="checkbox" checked><span>I approve');
  });

  it("keeps a secure local account and Headlamp handoff for Core-only", () => {
    expect(page).toContain("const wantsLocalAccount = () => target() === 'kind';");
    expect(page).toContain("const supported = () => target() === 'kind';");
    expect(page).toContain("Headlamp account email");
    expect(page).toContain("read-only dashboard access");
    expect(page).toContain("'Open Bridge →' : 'Open Headlamp →'");
    expect(page).toContain("Core-only uses a dedicated installer identity and short-lived credentials to enroll this provider");
    expect(page).not.toContain("Core-only enrollment is not available");
    expect(page).toContain("If you include Bridge, its orchestrator uses the same defaults.");
    expect(page).toContain("Core defaults for SRE, and for Bridge when installed.");
  });

  it("keeps bootstrap and credentials out of browser persistence and HTML interpolation", () => {
    expect(scripts[0]).not.toMatch(/localStorage|sessionStorage|indexedDB|document\.write|innerHTML|outerHTML|eval\(/);
    expect(scripts[0]).toContain("history.replaceState(null, '', location.pathname + location.search)");
    expect(scripts[0]).toContain("'X-CSRF-Token': csrf");
    expect(scripts[0]).toContain("credentials: 'same-origin'");
    expect(scripts[0]).toContain("title.textContent = check.label");
    expect(scripts[0]).toContain("detail.textContent = check.detail");
    expect(scripts[0]).toContain("url.protocol === 'https:'");
    expect(scripts[0]).toContain('!url.username && !url.password');
    expect(scripts[0]).toContain("['api-key','admin-password','admin-confirm'].forEach");
  });

  it("provides labelled responsive screens and truthful error states", () => {
    const ids = [...page.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]);
    expect(new Set(ids).size).toBe(ids.length);
    for (const [, id] of page.matchAll(/aria-labelledby="([^"]+)"/g)) expect(ids).toContain(id);
    expect(page).toContain('role="alert"');
    expect(page).toContain('role="status"');
    expect(page).toContain('@media (max-width: 760px)');
    expect(page).toContain(':focus-visible');
    expect(page).toContain("'Setup needs attention.'");
    expect(page).toContain("const ready = next.phase === 'ready'");
    expect(page).toContain('Installed. Sign in next.');
    expect(page).toContain('Installation status is unknown.');
    expect(page).toContain('no retry was submitted');
  });
});
