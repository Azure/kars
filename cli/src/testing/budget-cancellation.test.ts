// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const { apiRequest, withKindApi } = await import(
  new URL("../../../tests/e2e/budget-api-client.mjs", import.meta.url).href);
const { cancelAcceptedTask, cancellationFact } = await import(
  new URL("../../../tests/e2e/budget-cancellation.mjs", import.meta.url).href);
const root = fileURLToPath(new URL("../../../", import.meta.url));
const context = "kind-kars-e2e";
const namespace = "kars-system", name = "budget-token-left";
const taskPath = `/apis/kars.azure.com/v1alpha1/namespaces/${namespace}/karstasks/${name}`;
const secret = "DO-NOT-EXPORT-API-MESSAGE-BODY-OR-DETAILS";

function status(code = 409, reason = "Conflict") {
  return { apiVersion: "v1", kind: "Status", status: "Failure", code, reason,
    message: secret, details: { name: secret, causes: [{ message: secret }] } };
}

async function fixture() {
  const binding = { scope: "GovernedInference", taskUid: "task-uid",
    account: { namespace, name: "account", uid: "account-uid" },
    root: { workspaceUid: "workspace-uid" }, authorizationDigest: "fixed-authority" };
  const created = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask",
    metadata: { name, namespace, uid: "task-uid", resourceVersion: "1", generation: 1 },
    spec: { objective: "fixture", envelope: { budget: { scope: "GovernedInference", tokens: 50 } },
      blueprint: { instructions: "unchanged" }, execution: { launch: true } },
    status: { inferenceBudget: binding } };
  const state = {
    task: structuredClone(created), namespaceUid: "workspace-uid", patches: [] as any[],
    requests: [] as string[], failure: false, conflicts: 0,
    reply: undefined as { code: number; body: any } | undefined,
    afterConflict: () => {}, afterCommit: () => {}, loseResponse: false,
  };
  const server = createServer((request, response) => {
    void (async () => {
      state.requests.push(`${request.method} ${request.url}`);
      const send = (code: number, body: unknown) => {
        response.writeHead(code, { "Content-Type": "application/json" });
        response.end(typeof body === "string" ? body : JSON.stringify(body));
      };
      if (request.method === "GET" && request.url === `/api/v1/namespaces/${namespace}`)
        return send(200, { apiVersion: "v1", kind: "Namespace",
          metadata: { name: namespace, uid: state.namespaceUid } });
      if (request.method === "GET" && request.url === taskPath) return send(200, state.task);
      if (request.method !== "PATCH" || request.url !== taskPath) return send(404, status(404, "NotFound"));
      expect(request.headers["content-type"]).toBe("application/merge-patch+json");
      expect(request.headers["impersonate-user"]).toBeUndefined();
      const chunks: Buffer[] = [];
      for await (const chunk of request) chunks.push(Buffer.from(chunk));
      const patch = JSON.parse(Buffer.concat(chunks).toString());
      state.patches.push(patch);
      expect(patch).toEqual({ metadata: { uid: created.metadata.uid,
        resourceVersion: state.task.metadata.resourceVersion }, spec: { execution: { launch: false } } });
      if (state.reply) return send(state.reply.code, state.reply.body);
      if (state.conflicts-- > 0) {
        state.task.metadata.resourceVersion = String(Number(state.task.metadata.resourceVersion) + 1);
        state.afterConflict();
        return send(409, status());
      }
      state.task.spec.execution.launch = false;
      state.task.metadata.generation++;
      state.task.metadata.resourceVersion = String(Number(state.task.metadata.resourceVersion) + 1);
      state.afterCommit();
      if (state.loseResponse) return request.socket.destroy();
      send(200, state.task);
    })().catch(() => { state.failure = true; response.writeHead(500); response.end(); });
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Controlled API listener failed");
  const url = `http://127.0.0.1:${address.port}`;
  const facts: any[] = [];
  const options = { created, binding: structuredClone(binding), request: apiRequest(url),
    report: (fact: unknown) => facts.push(fact), deadline: Date.now() + 3000 };
  return { state, options, facts, url, close: async () => {
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    expect(state.failure).toBe(false);
  } };
}

async function kubeconfig(url: string, run: (file: string, directory: string) => Promise<void>) {
  const directory = mkdtempSync(join(root, ".budget-cancel-test-"));
  const file = join(directory, "config.json");
  writeFileSync(file, JSON.stringify({
    apiVersion: "v1", kind: "Config", "current-context": context,
    clusters: [{ name: "fixture", cluster: { server: url } }],
    contexts: [{ name: context, context: { cluster: "fixture", user: "fixture" } }],
    users: [{ name: "fixture", user: {} }],
  }), { mode: 0o600 });
  try { await run(file, directory); }
  finally { rmSync(directory, { recursive: true, force: true }); }
}

describe("native accepted-work cancellation API contract", () => {
  it("reproduces literal --patch-file '-' failure before any API request with real kubectl", async () => {
    const f = await fixture();
    try {
      await kubeconfig(f.url, async (file, directory) => {
        const child = spawn("kubectl", ["--kubeconfig", file, "--cache-dir", join(directory, "cache"),
          "--context", context, "patch", "karstask", name, "-n", namespace, "--type=merge", "--patch-file", "-"],
        { cwd: directory, stdio: ["pipe", "pipe", "pipe"] });
        let stderr = "";
        child.stderr.on("data", data => { stderr = (stderr + data).slice(-8192); });
        child.stdout.on("data", () => {});
        child.stdin.end(JSON.stringify({ metadata: { uid: "task-uid", resourceVersion: "1" },
          spec: { execution: { launch: false } } }));
        const code = await new Promise<number | null>((resolve, reject) => {
          child.once("close", resolve);
          child.once("error", reject);
        });
        expect(code).not.toBe(0);
        expect(/unable to read patch file:.*open -:/.test(stderr)).toBe(true);
        expect(f.state.requests).toEqual([]);
      });
    } finally { await f.close(); }
  });

  it.each(["conflict", "forbidden"])("uses a real kubectl proxy, exact %s status and owned cleanup", async mode => {
    const f = await fixture();
    f.state.conflicts = 1;
    if (mode === "forbidden") f.state.reply = { code: 403, body: status(403, "Forbidden") };
    const children: ReturnType<typeof spawn>[] = [];
    try {
      await kubeconfig(f.url, async (file, directory) => {
        const extra = ["--kubeconfig", file, "--cache-dir", join(directory, "cache")];
        const pending = withKindApi({
          root: directory, context, deadline: f.options.deadline,
          kubectl: (args: string[]) => execFileSync("kubectl", [...extra, "--context", context, ...args],
            { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 2000 }),
          spawnProcess: (binary: string, args: string[], options: any) => {
            const child = spawn(binary, [...extra, ...args], options);
            children.push(child);
            return child;
          },
        }, (request: any) => cancelAcceptedTask({ ...f.options, request }));
        if (mode === "forbidden") {
          await expect(pending).rejects.toThrow("cancellation-patch");
          expect(f.state.patches).toHaveLength(1);
          expect(f.facts.at(-1)).toMatchObject({ httpStatus: 403, reason: "Forbidden" });
        } else {
          const result = await pending;
          expect(result.metadata.uid).toBe("task-uid");
          expect(result.spec.execution.launch).toBe(false);
          expect(f.state.patches.map(patch => patch.metadata.resourceVersion)).toEqual(["1", "2"]);
          expect(f.facts.filter(fact => fact.verb === "PATCH").map(fact => [fact.httpStatus, fact.reason]))
            .toEqual([[409, "Conflict"], [200, "Success"]]);
        }
        expect(JSON.stringify(f.facts)).not.toContain(secret);
      });
      expect(children).toHaveLength(1);
      expect(children[0].signalCode).toBe("SIGTERM");
    } finally { await f.close(); }
  });

  it.each([[403, "Forbidden"], [422, "Invalid"], [429, "TooManyRequests"], [503, "ServiceUnavailable"]])(
    "retains actual HTTP %s and fails without retry", async (code, reason) => {
      const f = await fixture();
      f.state.reply = { code: Number(code), body: status(Number(code), String(reason)) };
      try {
        await expect(cancelAcceptedTask(f.options)).rejects.toThrow("cancellation-patch");
        expect(f.state.patches).toHaveLength(1);
        expect(f.state.task.spec.execution.launch).toBe(true);
        expect(f.facts.at(-1)).toMatchObject({ verb: "PATCH", httpStatus: code, reason });
        expect(JSON.stringify(f.facts)).not.toContain(secret);
      } finally { await f.close(); }
    },
  );

  it.each([
    { code: 403, body: status(409, "Conflict") },
    { code: 409, body: status(409, "Invalid") },
    { code: 409, body: status(403, "Conflict") },
    { code: 409, body: secret },
  ])("never manufactures a retry from mismatched or malformed Status", async reply => {
    const f = await fixture();
    f.state.reply = reply;
    try {
      await expect(cancelAcceptedTask(f.options)).rejects.toThrow();
      expect(f.state.patches).toHaveLength(1);
      expect(JSON.stringify(f.facts)).not.toContain(secret);
    } finally { await f.close(); }
  });

  it.each(["task-uid", "namespace-uid", "generation", "spec", "binding", "already-stopped"])(
    "rejects changed %s after a confirmed conflict instead of adopting it", async change => {
      const f = await fixture();
      f.state.conflicts = 1;
      f.state.afterConflict = () => {
        if (change === "task-uid") f.state.task.metadata.uid = "replacement";
        if (change === "namespace-uid") f.state.namespaceUid = "replacement";
        if (change === "generation") f.state.task.metadata.generation++;
        if (change === "spec") f.state.task.spec.blueprint.instructions = "changed";
        if (change === "binding") f.state.task.status.inferenceBudget.account.uid = "replacement";
        if (change === "already-stopped") f.state.task.spec.execution.launch = false;
      };
      try {
        await expect(cancelAcceptedTask(f.options)).rejects.toThrow("identity-or-intent");
        expect(f.state.patches).toHaveLength(1);
      } finally { await f.close(); }
    },
  );

  it("does not repeat a stale RV when the confirmed conflict yields no new version", async () => {
    const f = await fixture();
    f.state.conflicts = 1;
    f.state.afterConflict = () => { f.state.task.metadata.resourceVersion = "1"; };
    try {
      await expect(cancelAcceptedTask(f.options)).rejects.toThrow("without-fresh-version");
      expect(f.state.patches).toHaveLength(1);
    } finally { await f.close(); }
  });

  it("limits real conflicts to three attempts without removing UID/RV preconditions", async () => {
    const f = await fixture();
    f.state.conflicts = 10;
    try {
      await expect(cancelAcceptedTask(f.options)).rejects.toThrow("conflict-limit");
      expect(f.state.patches.map(patch => patch.metadata)).toEqual([
        { uid: "task-uid", resourceVersion: "1" }, { uid: "task-uid", resourceVersion: "2" },
        { uid: "task-uid", resourceVersion: "3" },
      ]);
    } finally { await f.close(); }
  });

  it("does not retry or claim success after a committed patch loses its response", async () => {
    const f = await fixture();
    f.state.loseResponse = true;
    try {
      await expect(cancelAcceptedTask(f.options)).rejects.toThrow("transport");
      expect(f.state.patches).toHaveLength(1);
      expect(f.state.task.spec.execution.launch).toBe(false);
      expect(f.facts.at(-1)).toMatchObject({ httpStatus: 0, reason: "Unclassified" });
    } finally { await f.close(); }
  });

  it("retains the original deadline and never claims a late successful response", async () => {
    const f = await fixture();
    let time = 0;
    f.state.afterCommit = () => { time = 100; };
    try {
      await expect(cancelAcceptedTask({ ...f.options, now: () => time, deadline: 100 }))
        .rejects.toThrow("deadline");
      expect(f.state.patches).toHaveLength(1);
      await expect(cancelAcceptedTask({ ...f.options, deadline: Date.now() - 1 })).rejects.toThrow("deadline");
      expect(f.state.patches).toHaveLength(1);
    } finally { await f.close(); }
  });

  it("does not treat a 200 response with wrong intent as successful cancellation", async () => {
    const f = await fixture();
    f.state.afterCommit = () => { f.state.task.spec.execution.launch = true; };
    try {
      await expect(cancelAcceptedTask(f.options)).rejects.toThrow("identity-or-intent");
      expect(f.state.patches).toHaveLength(1);
    } finally { await f.close(); }
  });

  it("exports only allowlisted status/reason facts and leaves funding assertions in place", () => {
    expect(cancellationFact("PATCH", "karstasks", 1, { status: 422,
      body: { ...status(422, secret), message: secret } })).toEqual({
      stage: "accepted-work-cancellation", verb: "PATCH", resource: "karstasks",
      attempt: 1, httpStatus: 422, reason: "Unclassified",
    });
    const harness = readFileSync(new URL("../../../tests/e2e/inference-budget-enforcement.mjs", import.meta.url), "utf8");
    expect(harness).not.toContain('"--patch-file", "-"');
    expect(harness).toContain('created: createdTasks.get("budget-token-left"), binding');
    expect(harness).toContain("const cancellationDeadline = Date.now() + 30_000");
    expect(harness).toContain("assert.equal(final.status.ledger.meters.settled.tokens, 8)");
    expect(harness).toContain("assert.equal(final.status.ledger.meters.uncertain.tokens, 30)");
    expect(harness).toContain("assert.equal(final.status.ledger.meters.reserved.tokens, 0)");
  });
});
