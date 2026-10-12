// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { describe, expect, it } from "vitest";

// Execute only authentication, never the gateway, filesystem setup, or external curl.
const entrypoint = readFileSync(new URL("../../../../sandbox-images/openclaw/entrypoint.sh", import.meta.url), "utf8");
const start = entrypoint.indexOf('if [ "${MESH_AUTH_BACKEND:-}" = "EntraAgentIdentity" ]; then');
const end = entrypoint.indexOf("# Get config from env vars", start);
const block = entrypoint.slice(start, end);
const body = JSON.stringify({ access_token: "secret-fresh-token", token_type: "Bearer", expires_in: 3000 });
function run(overrides: NodeJS.ProcessEnv = {}) {
  return spawnSync("/bin/bash", ["-s"], {
    encoding: "utf8", timeout: 10_000,
    env: {
      PATH: process.env.PATH,
      MESH_AUTH_BACKEND: "EntraAgentIdentity", MESH_TOKEN_MAX_WAIT: "5",
      TEST_BODY: body, TEST_STATUS: "200", AGT_TRUST_THRESHOLD: "500",
      ...overrides,
    },
    input: `set -eu
exec 3>&2
curl() {
  printf 'REQUEST:%s\\n' "$*" >&3
  if [ "\${TEST_CURL_FAILURE:-0}" = 1 ]; then return 7; fi
  printf '%s\\n__HTTP_STATUS__%s\\n' "$TEST_BODY" "$TEST_STATUS"
}
sleep() { SECONDS=$((SECONDS + $1)); }
${block}
printf 'COMPLETE:token=%s;trust=%s;router=%s\\n' "\${AGT_OAUTH_TOKEN:-unset}" "\${AGT_TRUST_THRESHOLD:-unset}" "\${KARS_ROUTER_URL:-unset}"
`,
  });
}

describe("OpenClaw entrypoint required Entra preflight", () => {
  it("extracts the maintained auth block", () => { expect(start).toBeGreaterThan(0); expect(end).toBeGreaterThan(start); });
  it("requires a fresh mint even with a stale environment credential, then discards both", () => {
    const result = run({ AGT_OAUTH_TOKEN: "secret-stale-token", KARS_ROUTER_URL: "http://router:8443/" });
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("COMPLETE:token=unset;trust=500;router=http://router:8443/");
    expect(result.stderr).toContain("--connect-timeout 3 --max-time 5 --max-filesize 65536");
    expect(result.stderr).toContain("http://router:8443/v1/mesh-token");
    expect(result.stdout + result.stderr).not.toContain("secret-");
  });
  it.each(["AGT_SKIP_INIT", "AGT_SKIP_ENTRA", "AGT_SKIP_VERIFY"])("rejects %s without sending a request", (flag) => {
    const result = run({ [flag]: "1" });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("conflicts");
    expect(result.stderr).not.toContain("REQUEST:");
  });
  it.each([
    { TEST_STATUS: "404" }, { TEST_STATUS: "503" }, { TEST_CURL_FAILURE: "1" },
    { TEST_BODY: "not-json" }, { TEST_BODY: "{}" },
    { TEST_BODY: JSON.stringify({ access_token: "secret-token", token_type: "Basic", expires_in: 3000 }) },
    { TEST_BODY: JSON.stringify({ access_token: "secret-token", token_type: "Bearer", expires_in: 60 }) },
    { TEST_BODY: JSON.stringify({ access_token: "secret-token", token_type: "Bearer", expires_in: true }) },
    { TEST_BODY: JSON.stringify({ access_token: "secret-token", token_type: "Bearer", expires_in: 3000, success: false }) },
  ])("never downgrades or continues after token service failure %#", (scenario) => {
    const result = run({ ...scenario, AGT_OAUTH_TOKEN: "secret-stale-token" });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("refusing anonymous mesh startup");
    expect(result.stdout).not.toContain("COMPLETE:");
    expect(result.stdout + result.stderr).not.toContain("secret-");
    expect((result.stderr.match(/REQUEST:/g) ?? []).length).toBeLessThanOrEqual(3);
  });
  it.each(["0", "301", "bad", "-1"])("rejects invalid readiness wait %s", (wait) => {
    expect(run({ MESH_TOKEN_MAX_WAIT: wait }).status).toBe(1);
  });
  it("preserves legacy optional OAuth without required minting", () => {
    const result = run({ MESH_AUTH_BACKEND: "", AGT_OAUTH_TOKEN: "optional-token" });
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("COMPLETE:token=optional-token;trust=500");
    expect(result.stderr).not.toContain("REQUEST:");
  });
  it("preserves explicit anonymous development and clears an inherited credential", () => {
    const result = run({ MESH_AUTH_BACKEND: "", AGT_SKIP_ENTRA: "1", AGT_OAUTH_TOKEN: "stale" });
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("COMPLETE:token=unset;trust=0");
    expect(result.stderr).not.toContain("REQUEST:");
  });
});
