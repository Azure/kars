// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { access, cp, mkdir, readFile, readdir, realpath, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { createServer as createSecureServer, request as requestHttps } from "node:https";
import { dirname, join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";

const version = "2026.9.8";
const commit = "fc23bc864e4553c2d215e479eeec47b67a0bf943";
const pluginId = "kars-native-host-verification";
const model = "kars-native-host-verification-model";
const localProvider = "azure-openai";
const routerProvider = "named-router-verification-provider";
const names = ["kars_mission_file_read", "kars_mission_file_write", "kars_mission_http_fetch", "web_fetch", "kars_mission_mcp_list", "kars_mission_mcp_call"];
const nativeWebUrl = "https://native-host.invalid/pricing";
const readableMarker = "Verified native readable vendor article";
const fixtureDirectory = dirname(fileURLToPath(import.meta.url));
const runtimeDirectory = resolve(fixtureDirectory, "../..");
const adapterMode = process.env.KARS_NATIVE_HOST_ADAPTER_MODE ?? "source";
const adapterRoot = adapterMode === "installed" ? "/opt/kars-plugin" : join(runtimeDirectory, "src");

async function optionalJson(path) {
  try { return JSON.parse(await readFile(path, "utf8")); }
  catch (error) { if (error.code === "ENOENT") return undefined; throw error; }
}

async function preflight() {
  assert.ok(["source", "installed"].includes(adapterMode), "Unknown adapter mode; refusing source fallback");
  if (adapterMode === "installed") {
    const manifest = await optionalJson(join(adapterRoot, "package.json"));
    assert.equal(manifest?.name, "@kars/runtime-openclaw");
    await access(join(adapterRoot, "core/native-mission.js"));
  }
  const [major, minor] = process.versions.node.split(".").map(Number);
  assert.ok((major === 24 && minor >= 16) || major > 26 || (major === 26 && minor >= 1),
    `Pinned OpenClaw requires Node >=24.16.0 <25 || >=26.1.0; received ${process.versions.node}`);
  assert.ok(process.env.OPENCLAW_NATIVE_HOST_ROOT, "Set OPENCLAW_NATIVE_HOST_ROOT to the already-built pinned OpenClaw package root");
  const root = await realpath(process.env.OPENCLAW_NATIVE_HOST_ROOT);
  const packageInfo = await optionalJson(join(root, "package.json"));
  assert.equal(packageInfo?.name, "openclaw");
  assert.equal(packageInfo?.version, version);
  const build = await optionalJson(join(root, "dist/build-info.json"));
  assert.equal(build?.version, version, "Actual dist/build-info.json must identify the pinned build");
  assert.equal(build?.commit, commit, "Actual built artifact must contain the complete pinned commit; an environment assertion is insufficient");
  for (const directory of [root, join(root, "dist")]) {
    const stamp = await optionalJson(join(directory, ".buildstamp"));
    const postbuild = await optionalJson(join(directory, ".runtime-postbuildstamp"));
    if (stamp || postbuild) {
      assert.equal(stamp?.head, commit, "Build stamp must agree with loaded artifact provenance");
      assert.equal(postbuild?.head, commit, "Postbuild stamp must agree with loaded artifact provenance");
    }
  }
  for (const pending of [".openclaw-lifecycle-pending", "dist/openclaw-install-guard"]) {
    await access(join(root, pending)).then(
      () => { throw new Error(`Refusing unfinished package lifecycle: ${pending}`); },
      error => { if (error.code !== "ENOENT") throw error; },
    );
  }
  await access(join(root, "openclaw.mjs"));
  await access(join(root, "dist/entry.js")).catch(async error => {
    if (error.code !== "ENOENT") throw error;
    await access(join(root, "dist/entry.mjs"));
  });
  const sqlite = await import(pathToFileURL(join(root, "node-sqlite.mjs")).href);
  assert.equal(sqlite.nodeRuntimeFailure(process.versions.node, await sqlite.detectCurrentSqliteCapabilities()), null,
    "Pinned launcher must not attempt Node recovery or installation");
  const physicalRuntime = await realpath(runtimeDirectory);
  assert.ok(physicalRuntime.startsWith("/sandbox/"),
    "Mount a writable physical checkout beneath /sandbox; the real filesystem helper rejects other roots");
  assert.equal(physicalRuntime, runtimeDirectory, "Use a non-symlink checkout so real descriptor-relative file checks remain meaningful");
  await access("/usr/bin/python3");
  await access(join(adapterRoot, "core/task-filesystem.py"));
  return root;
}

async function listen(server) {
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  return server.address().port;
}

async function close(server) {
  server.closeAllConnections();
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
}

async function readBody(request) {
  let body = "";
  for await (const chunk of request) {
    body += chunk;
    assert.ok(Buffer.byteLength(body) <= 2 * 1024 * 1024, "Unexpectedly large model/router request");
  }
  return body ? JSON.parse(body) : undefined;
}

function json(response, body, status = 200) {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(body));
}

function createRouterBoundary({ scope, assignment, inputPath, inputText, outputPath, outputText, fetchedText, fetchNative, copyInput, copyPath }) {
  let sequence = 40;
  let fetchAttempts = 0;
  let nativeFetchAttempts = 0;
  const events = [];
  const requests = [];
  const errors = [];
  const modelRequests = [];
  const authorizations = new Map();
  const outcomes = new Map();
  const remoteUrl = "https://native-host.invalid/reference";
  const plan = [
    { id: "nativeread", name: names[0], arguments: { path: inputPath } },
    { id: "nativefetchfailed", name: names[2], arguments: { method: "GET", url: remoteUrl } },
    { id: "nativefetchrecovered", name: names[2], arguments: { method: "GET", url: remoteUrl } },
    { id: "nativewebdenied", name: "web_fetch", arguments: { url: nativeWebUrl, extractMode: "markdown", maxChars: 2000 } },
    { id: "nativeweb", name: "web_fetch", arguments: { url: nativeWebUrl, extractMode: "markdown", maxChars: 2000 } },
    { id: "nativewrite", name: names[1], arguments: { path: outputPath, content: outputText, artifact_name: "report.md" } },
    { id: "nativecopywrong", name: names[1], arguments: { path: copyPath,
      source_path: `/sandbox/.kars-inputs/${copyInput.name}`, sha256: `sha256:${"0".repeat(64)}`, artifact_name: copyInput.name } },
    { id: "nativecopy", name: names[1], arguments: { path: copyPath,
      source_path: `/sandbox/.kars-inputs/${copyInput.name}`, sha256: copyInput.sha256, artifact_name: copyInput.name } },
  ];
  const add = event => events.push({ scope_id: scope, seq: ++sequence, ...event });
  const server = createServer(async (request, response) => {
    try {
      const url = new URL(request.url, "http://127.0.0.1");
      const body = await readBody(request);
      requests.push({ method: request.method, path: url.pathname, query: url.search, body });
      if (url.pathname === "/v1/models") {
        assert.equal(request.method, "GET");
        assert.equal(modelRequests.length, 0);
        return json(response, { object: "list", data: [{ id: model,
          context_window: 1_050_000, max_output_tokens: 128_000 }] });
      }
      if (url.pathname === "/telemetry/cursor") {
        assert.equal(request.method, "GET");
        assert.equal(modelRequests.length, 0, "Scope must be captured before native inference");
        return json(response, { scope_id: scope, cursor: sequence, durable: false });
      }
      if (url.pathname === "/telemetry/trace") {
        assert.equal(request.method, "GET");
        if (request.headers["x-kars-service-scope"] !== scope) {
          return json(response, { error: "stale_scope" }, 409);
        }
        const since = Number(url.searchParams.get("since"));
        assert.ok(url.searchParams.has("since") && Number.isSafeInteger(since) && since >= 40 && since <= sequence,
          "Trace cursor must stay within the issued scope's observed high-water mark");
        assert.ok(since === 40 || events.some(event => event.kind === "round" && event.seq === since + 1
          && event.round < 1000), "Advancing correlation must retain its previously matched proposing round");
        assert.equal(url.searchParams.has("rounds"), false);
        // This is the cursor API, not a selected-round snapshot: there is no `truncated` field.
        return json(response, { scope_id: scope, cursor: sequence, high_water: sequence,
          has_more: false, dropped_events: 0, coverage: "router-observed-only", durable: false,
          events: events.filter(event => event.seq > since) });
      }
      if (url.pathname === "/agt/evaluate") {
        assert.equal(request.method, "POST");
        const ctx = body.context;
        const proposed = events.filter(event => event.kind === "tool_proposed" && event.call_id === ctx.tool_call_id);
        assert.equal(proposed.length, 1, "Authorize only an exact observed tool proposal");
        const step = plan.find(step => step.id === ctx.tool_call_id);
        assert.ok(step, "Never authorize unrelated concurrent rounds");
        assert.equal(body.action, `tool:${step.name}`);
        assert.equal(ctx.tool, proposed[0].name);
        assert.deepEqual(ctx.arguments, step.arguments);
        assert.equal(ctx.scope_id, scope);
        assert.equal(ctx.task_uid, assignment.taskUid);
        assert.equal(ctx.run_nonce, assignment.runNonce);
        assert.equal(ctx.assignment_id, assignment.assignmentId);
        assert.equal(ctx.native_provider, localProvider);
        assert.equal(ctx.router_provider, routerProvider);
        assert.equal(ctx.native_model, model);
        assert.ok(ctx.native_run_id && ctx.native_session_id);
        assert.equal(authorizations.has(step.id), false, "Do not reauthorize or replay a physical effect");
        if (step.id === "nativewebdenied") return json(response, { allowed: false });
        authorizations.set(step.id, body);
        return json(response, { allowed: true });
      }
      if (url.pathname === "/telemetry/tool") {
        assert.equal(request.method, "POST");
        assert.equal(body.scope_id, scope);
        assert.ok(authorizations.has(body.call_id), "Outcome requires a matching genuine authorization");
        assert.equal(outcomes.has(body.call_id), false, "Outcome must be reported once");
        assert.equal(typeof body.ok, "boolean");
        assert.ok(Number.isFinite(body.latency_ms) && body.latency_ms >= 0);
        outcomes.set(body.call_id, body);
        const proposal = events.find(event => event.kind === "tool_proposed" && event.call_id === body.call_id);
        add({ kind: "tool_result", round: proposal.round, source: "harness-reported", call_id: body.call_id,
          name: proposal.name, ok: body.ok, ms: body.latency_ms,
          ...(body.result_summary ? { result_summary: body.result_summary } : {}) });
        return json(response, { recorded: true, source: "harness-reported", enforcement_changed: false });
      }
      if (url.pathname === "/egress/fetch") {
        assert.equal(request.method, "POST");
        if (body.url === nativeWebUrl) {
          assert.ok(authorizations.has("nativeweb"), "Mediated native retrieval must follow exact authorization");
          assert.equal(++nativeFetchAttempts, 1, "Denied native invocation must not reach mediated fetch");
          assert.deepEqual(body, { method: "GET", url: nativeWebUrl, headers: {
            Accept: "text/markdown, text/html;q=0.9, text/plain;q=0.8, */*;q=0.1",
          }, telemetry: { scope_id: scope, call_id: "nativeweb" } });
          return json(response, await fetchNative());
        }
        const id = fetchAttempts++ === 0 ? "nativefetchfailed" : "nativefetchrecovered";
        assert.deepEqual(body, { method: "GET", url: remoteUrl,
          telemetry: { scope_id: scope, call_id: id } });
        assert.ok(authorizations.has(id), "Network request preceded policy authorization");
        assert.ok(fetchAttempts <= 2, "Native loop, not a hidden HTTP retry, owns recovery");
        return json(response, { status: fetchAttempts === 1 ? 503 : 200,
          headers: { "content-type": "text/plain" }, body: fetchAttempts === 1 ? "Try again" : fetchedText, truncated: false });
      }
      assert.equal(url.pathname, "/v1/chat/completions", `Unexpected HTTP boundary ${request.method} ${url.pathname}`);
      assert.equal(request.method, "POST");
      assert.equal(body.model, model);
      assert.equal(body.max_tokens ?? body.max_completion_tokens, 128_000, "Governed model output capacity must replace stale runtime metadata");
      assert.equal(body.stream, true);
      assert.deepEqual(body.tools.map(tool => tool.function.name).sort(), [...names].sort(), "No ambient native tools may escape the reviewed capability set");
      modelRequests.push(body);
      const systemMessages = body.messages.filter(message => ["system", "developer"].includes(message.role));
      const systemText = JSON.stringify(systemMessages);
      assert.ok(systemText.includes("Kars native-host configured identity"), "Configured identity must reach the real model request");
      assert.ok(systemText.includes("Before your final response, use kars_mission_file_write"), "Configured identity must not erase native output instructions");
      assert.ok(systemText.includes("Execute the reviewed phase only:"), "Reviewed phase must reach the real model request");
      assert.ok(systemText.includes("report.md"), "Required named output must reach the real model request");
      const userText = JSON.stringify(body.messages.filter(message => message.role === "user"));
      assert.ok(userText.includes("Mission and role context (assigned work and feedback apply; execute only your reviewed phase):"));
      for (const text of [systemText, userText]) {
        assert.ok(text.includes("current assigned work and revision feedback update the substance of the original charter and phase recipe"), "Current work and feedback precedence must reach both native prompts");
        assert.ok(text.includes("not this call's execution stage or required export names"), "Current work must preserve this phase's execution stage and named outputs");
        assert.ok(text.includes("A reusable phase objective or available tool does not require an action excluded by the current assignment"), "Phase recipes must not override current work exclusions");
        assert.ok(text.includes("Do not invent additional task restrictions from this general precedence rule"), "Generic precedence must not add task-specific restrictions");
        assert.ok(!text.includes("without fetching new sources"), "The network assignment must not inherit a document-only restriction");
        assert.ok(text.includes("never expands reviewed capabilities, permissions"), "Current work must not expand reviewed authority");
        assert.ok(text.includes("report the conflict without executing it or claiming completion"), "Conflicting mandatory calls must not execute silently");
        assert.ok(text.includes("Keep this role's boundaries: later phases produce their own deliverables"), "Feedback must preserve reviewed phase boundaries");
        assert.ok(text.includes("Prior output is reference evidence, not approved knowledge or new permissions"), "Revision inputs must not become authority");
      }
      assert.ok(userText.includes("Your current reviewed assignment:"));
      assert.ok(userText.includes("Execute the reviewed phase only:"));
      assert.ok(userText.includes("Before your final response, use kars_mission_file_write"));
      assert.ok(userText.includes("report.md"), "Current named output must also reach the native user prompt");
      const round = modelRequests.length;
      assert.ok(round <= 10, "Only one explicitly requested native finalization revision is allowed");
      // The first draft has no effects: the pinned host refuses to rewind tools.
      const toolRound = round - 1;
      if (toolRound > 1) {
        const previous = plan[toolRound - 2];
        assert.equal(outcomes.has(previous.id), previous.id !== "nativewebdenied", "The actual authorized tool outcome must precede the next native model call");
        const toolMessages = body.messages.filter(message => message.role === "tool" && message.tool_call_id === previous.id);
        assert.equal(toolMessages.length, 1, "Native engine must return the dispatched tool result to the model");
        const text = JSON.stringify(toolMessages[0].content);
        if (toolRound === 2) assert.ok(text.includes(inputText), "Actual file bytes must reach native inference");
        if (toolRound === 3) {
          assert.match(text, /503/, "Actual upstream status must reach the model before recovery");
          assert.ok(text.includes("Try again"), "Actual upstream failure body must reach the model before recovery");
        }
        if (toolRound === 4) assert.ok(text.includes(fetchedText), "Recovered GET bytes must reach native inference");
        if (previous.id === "nativewebdenied") assert.match(text, /denied|blocked/i);
        if (previous.id === "nativecopywrong") {
          assert.equal(outcomes.get(previous.id).ok, false, "Wrong digest must not report success");
          assert.match(text, /exact sha256/, "Digest refusal must reach the model before a fresh corrected copy");
          await assert.rejects(readFile(copyPath), { code: "ENOENT" }, "Rejected copy must not create an export file");
        }
        if (previous.id === "nativecopy") {
          assert.equal(outcomes.get(previous.id).ok, true);
          assert.ok(text.includes(copyInput.sha256), "Successful copy must return the retained input digest");
        }
        if (previous.id === "nativeweb") {
          assert.ok(text.includes(readableMarker), "Native readable output must reach the model");
          assert.ok(text.length < 12000, "Bounded native content must not send the raw page into model context");
          assert.ok(!text.includes("RAW_SCRIPT_SENTINEL"), "Page scripts must not enter readable content");
        }
      }
      const step = plan[toolRound - 1];
      const observedRound = { kind: "round", source: "router-upstream", provider: routerProvider, model, accepted: true,
        http_status: 200, outcome: "response_observed", usage_state: "present", partial_observation: false,
        usage: { prompt_tokens: 100 + round, completion_tokens: 10 + round, total_tokens: 110 + 2 * round },
        ms: 1, tool_calls_observed: step ? 1 : 0 };
      add({ ...observedRound, round });
      if (step) add({ kind: "tool_proposed", source: "model-proposed", round, call_id: step.id, name: step.name });
      // Deliberately interleaved calls share scope and model but must never be attributed to this Mission.
      add({ ...observedRound, round: 1000 + round });
      add({ kind: "tool_proposed", source: "model-proposed", round: 1000 + round, call_id: `unrelated_${round}`, name: names[0] });
      const frame = delta => ({ id: `chatcmpl_native_${round}`, object: "chat.completion.chunk",
        created: 1_800_000_000, model, choices: [{ index: 0, delta, finish_reason: null }] });
      response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      const send = data => response.write(`data: ${JSON.stringify(data)}\n\n`);
      send(frame({ role: "assistant", content: "" }));
      send(frame(step ? { tool_calls: [{ index: 0, id: step.id, type: "function",
        function: { name: step.name, arguments: JSON.stringify(step.arguments) } }] }
        : { content: round === 1 ? "Draft before the assigned tools have run." : "Native host file and governed GET recovery completed." }));
      send({ ...frame({}), choices: [{ index: 0, delta: {}, finish_reason: step ? "tool_calls" : "stop" }] });
      send({ ...frame({}), choices: [], usage: { prompt_tokens: 100 + round, completion_tokens: 10 + round,
        total_tokens: 110 + 2 * round, prompt_tokens_details: { cached_tokens: 4 } } });
      response.end("data: [DONE]\n\n");
    } catch (error) {
      errors.push(error.stack ?? String(error));
      if (!response.headersSent) json(response, { error: { message: error.message } }, 500);
      else response.destroy(error);
    }
  });
  return { server, errors, requests, modelRequests, authorizations, outcomes, plan, events,
    get fetchAttempts() { return fetchAttempts; }, get nativeFetchAttempts() { return nativeFetchAttempts; } };
}

async function stopChild(child, exited) {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;
  child.kill("SIGTERM");
  if (!await Promise.race([exited.then(() => true), delay(10_000, undefined, { ref: false }).then(() => false)])) {
    child.kill("SIGKILL");
    await exited;
  }
}

test("genuine pinned PluginRuntime runs Mission tools, governed GET recovery, and measured evidence", { timeout: 180_000 }, async t => {
  const hostRoot = await preflight();
  const cleanup = [];
  t.after(async () => {
    const errors = [];
    for (const dispose of cleanup.reverse()) {
      try { await dispose(); } catch (error) { errors.push(error); }
    }
    if (errors.length) throw new AggregateError(errors, "Native host cleanup failed");
  });
  const root = join(fixtureDirectory, ".runs", randomUUID());
  await mkdir(root, { recursive: true, mode: 0o700 });
  // Keep imported source and non-module helpers in one admitted package, as in the installed adapter.
  const pluginPackage = join(root, "plugin-package");
  await mkdir(join(pluginPackage, "tests/native-host/plugin"), { recursive: true });
  await cp(join(adapterRoot, "core"), join(pluginPackage, "src/core"), { recursive: true });
  const adapterInputs = {};
  async function verifyCopiedCore(relative = "core") {
    for (const entry of await readdir(join(adapterRoot, relative), { withFileTypes: true })) {
      const name = join(relative, entry.name);
      if (entry.isDirectory()) { await verifyCopiedCore(name); continue; }
      assert.ok(entry.isFile(), `Unexpected adapter link or special file: ${name}`);
      const original = await readFile(join(adapterRoot, name));
      assert.deepEqual(await readFile(join(pluginPackage, "src", name)), original);
      adapterInputs[name] = createHash("sha256").update(original).digest("hex");
    }
  }
  await verifyCopiedCore();
  await writeFile(join(root, "adapter-inputs.json"), JSON.stringify({ mode: adapterMode, root: adapterRoot, files: adapterInputs }, null, 2));
  await cp(join(fixtureDirectory, "plugin/index.ts"), join(pluginPackage, "tests/native-host/plugin/index.ts"));
  await cp(join(fixtureDirectory, "plugin/openclaw.plugin.json"), join(pluginPackage, "openclaw.plugin.json"));
  const pluginManifest = JSON.parse(await readFile(join(fixtureDirectory, "plugin/package.json"), "utf8"));
  pluginManifest.openclaw.extensions = ["./tests/native-host/plugin/index.ts"];
  await writeFile(join(pluginPackage, "package.json"), JSON.stringify(pluginManifest));
  const workspace = join(root, "workspace");
  for (const part of ["workspace", "state", "home", "cache"]) await mkdir(join(root, part), { mode: 0o700 });
  const inputPath = join(workspace, "input.txt");
  const outputPath = join(workspace, "report.md");
  const inputText = `real-file-${randomUUID()}-café`;
  const fetchedText = `real-governed-get-${randomUUID()}`;
  const outputText = `# Native host report\n\n${inputText}\n\n${fetchedText}\n`;
  await writeFile(inputPath, inputText);
  const copyText = "\ufeff# café — 雪\r\nprint('naïve 🧪')\n# combining: e\u0301\r\n";
  const copyInput = { name: "app.py", content: copyText,
    sha256: `sha256:${createHash("sha256").update(Buffer.from(copyText, "utf8")).digest("hex")}`,
    source: { namespace: "kars-system", taskName: "native-host-builder", taskUid: randomUUID(),
      sandboxUid: randomUUID(), podUid: randomUUID(), runNonce: randomUUID(), assignmentId: randomUUID(),
      agentDid: `did:mesh:${"c".repeat(32)}`, artifactName: "app.py" } };
  const copyPath = join(workspace, copyInput.name);
  const assignment = { type: "mission:assign", version: 3, taskName: "native-host-verification",
    taskUid: randomUUID(), sandboxUid: randomUUID(), podUid: randomUUID(),
    agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}`,
    runNonce: randomUUID(), assignmentId: randomUUID(), bootId: randomUUID(),
    content: `Read ${inputPath}, obtain https://native-host.invalid/reference through the router (recover if transiently unavailable), and write ${outputPath}. Also copy the retained app.py input unchanged to ${copyPath} using source_path and its expected sha256; correct a refused digest with a fresh tool call. Return report.md, app.py and a final summary.`,
    inputArtifacts: [copyInput],
    artifactFormat: "text-v1", activityFormat: "router-observations-v1", requiredOutputs: ["report.md"],
    reviewedPhase: { name: "native-host-verification", objective: "Verify native file and governed network operations",
      executionMode: "native", capabilities: ["filesystem-read", "filesystem-write", "network"],
      minToolCalls: 0, maxToolCalls: 8, freshContext: false,
      requiredToolCalls: [{ name: "http_fetch", arguments: { method: "GET", url: "https://native-host.invalid/reference" } }] } };
  await writeFile(join(root, "assignment.json"), JSON.stringify(assignment));
  const boundary = createRouterBoundary({ scope: `native-host-${randomUUID()}`, assignment, inputPath, inputText, outputPath, outputText, fetchedText, copyInput, copyPath,
    fetchNative: () => new Promise((resolve, reject) => {
      const upstream = requestHttps({ hostname: "127.0.0.1", port: tlsPort, servername: "native-host.invalid",
        ca: tlsCertificate, path: "/pricing", method: "GET", agent: false }, response => {
        const chunks = [];
        let bytes = 0;
        response.on("data", chunk => {
          bytes += chunk.length;
          if (bytes > 2 * 1024 * 1024) upstream.destroy(new Error("Fixture exceeded router response cap"));
          else chunks.push(chunk);
        });
        response.on("error", reject);
        response.on("end", () => resolve({ status: response.statusCode, headers: response.headers,
          body: Buffer.concat(chunks).toString("utf8"), truncated: false }));
      });
      upstream.setTimeout(10_000, () => upstream.destroy(new Error("Fixture TLS request timed out")));
      upstream.on("error", reject);
      upstream.end();
    }) });
  const port = await listen(boundary.server);
  cleanup.push(() => close(boundary.server));
  assert.ok(process.env.KARS_NATIVE_HOST_TLS_DIR, "Provide a local test CA certificate/key for native-host.invalid");
  const tlsDirectory = process.env.KARS_NATIVE_HOST_TLS_DIR;
  const tlsCertificate = await readFile(join(tlsDirectory, "cert.pem"));
  let nativeRequests = 0;
  let proxyRequests = 0;
  const tlsServer = createSecureServer({
    key: await readFile(join(tlsDirectory, "key.pem")),
    cert: await readFile(join(tlsDirectory, "cert.pem")),
  }, (request, response) => {
    try {
      assert.ok(boundary.authorizations.has("nativeweb"), "Native network effect must follow router authorization");
      assert.equal(request.url, "/pricing");
      assert.equal(request.method, "GET");
      nativeRequests++;
      response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      response.end(`<html><head><title>Vendor pricing</title><script>${"RAW_SCRIPT_SENTINEL ".repeat(20000)}</script></head><body><main><article><h1>${readableMarker}</h1>${`<p>${readableMarker}. Annual support pricing is twelve seats at twenty euros per seat per month. AI resolutions are billed separately; confirm residency before purchase.</p>`.repeat(1000)}</article></main></body></html>`);
    } catch (error) {
      boundary.errors.push(error.stack ?? String(error));
      response.writeHead(500).end();
    }
  });
  const tlsPort = await listen(tlsServer);
  cleanup.push(() => close(tlsServer));
  const tunnels = new Set();
  const proxy = createServer((_request, response) => response.writeHead(403).end());
  proxy.on("connect", (request, socket) => {
    tunnels.add(socket);
    socket.on("close", () => tunnels.delete(socket));
    socket.on("error", () => socket.destroy());
    try {
      assert.ok(boundary.authorizations.has("nativeweb"), "CONNECT must follow router authorization");
      assert.equal(request.url, "native-host.invalid:443");
      proxyRequests++;
      const body = "Opaque tunnels are unsupported for governed inference; use mediated router APIs";
      socket.end(`HTTP/1.1 403 Forbidden\r\nContent-Length: ${Buffer.byteLength(body)}\r\nConnection: close\r\n\r\n${body}`);
    } catch (error) {
      boundary.errors.push(error.stack ?? String(error));
      socket.destroy();
    }
  });
  await new Promise((resolve, reject) => {
    proxy.once("error", reject);
    proxy.listen(8444, "127.0.0.1", resolve);
  });
  cleanup.push(async () => { for (const stream of tunnels) stream.destroy(); await close(proxy); });
  const reservation = createServer();
  const gatewayPort = await listen(reservation);
  await close(reservation);
  const token = randomUUID();
  const entrypointPath = adapterMode === "installed" ? "/usr/local/bin/entrypoint.sh"
    : resolve(runtimeDirectory, "../../sandbox-images/openclaw/entrypoint.sh");
  const entrypoint = await readFile(entrypointPath, "utf8");
  const bootstrapStart = entrypoint.indexOf("  # Write openclaw.json (");
  const bootstrapEnd = entrypoint.indexOf("  # Build channels config dynamically", bootstrapStart);
  assert.ok(bootstrapStart > 0 && bootstrapEnd > bootstrapStart, "Missing production provider initialization");
  const bootstrap = spawnSync("/bin/bash", ["-e", "-u", "-o", "pipefail", "-s"], {
    input: entrypoint.slice(bootstrapStart, bootstrapEnd), encoding: "utf8", timeout: 10_000,
    env: { PATH: process.env.PATH, OPENCLAW_DIR: join(root, "state"),
      OPENCLAW_CONFIG: join(root, "bootstrap.json"), MODELS_JSON: JSON.stringify([{ id: model }]),
      _PRIMARY_MODEL_REF: `${localProvider}/${model}`, _ANTHROPIC_PROVIDER_BLOCK: "", _MCP_BLOCK: "", IS_ROOT: "false" },
  });
  assert.equal(bootstrap.status, 0, bootstrap.stderr);
  const productionConfig = JSON.parse((await readFile(join(root, "bootstrap.json"), "utf8"))
    .replace("PLUGINS_ALLOW_PLACEHOLDER", "").replace("PLUGINS_ENTRIES_PLACEHOLDER", "")
    .replace("CHANNELS_PLACEHOLDER", ""));
  await writeFile(join(root, "bootstrap-sha256.txt"), createHash("sha256").update(entrypoint).digest("hex"));
  const config = {
    gateway: { mode: "local", bind: "loopback", port: gatewayPort, auth: { mode: "token", token }, controlUi: { enabled: false } },
    logging: { level: "debug", file: join(root, "gateway.log") },
    agents: { defaults: { workspace, model: { primary: `${localProvider}/${model}` }, heartbeat: { every: "0m" } } },
    models: { mode: "replace", providers: { [localProvider]: {
      ...productionConfig.models.providers[localProvider], baseUrl: `http://127.0.0.1:${port}/v1`,
      models: [{ id: model, name: model, reasoning: false, input: ["text"], contextWindow: 32768,
        maxTokens: 1024, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
    } } },
    tools: { allow: names, toolSearch: false, web: productionConfig.tools.web },
    plugins: { enabled: true, allow: [pluginId], load: { paths: [pluginPackage] },
      slots: { memory: "none" }, entries: { [pluginId]: { enabled: true, hooks: { allowConversationAccess: true, allowPromptInjection: true } } } },
  };
  await writeFile(join(root, "openclaw.json"), JSON.stringify(config));
  const capacityStart = entrypoint.indexOf("# Prepare governed capacity before");
  const capacityEnd = entrypoint.indexOf("# ── Offload idle timeout enforcement", capacityStart);
  assert.ok(capacityStart > 0 && capacityEnd > capacityStart, "Missing production model capacity initialization");
  const capacityModule = adapterMode === "installed" ? "/opt/kars-plugin/core/model-limits.js"
    : join(runtimeDirectory, "dist/core/model-limits.js");
  await access(capacityModule);
  const capacityScript = entrypoint.slice(capacityStart, capacityEnd)
    .replace("'/opt/kars-plugin/core/model-limits.js'", JSON.stringify(pathToFileURL(capacityModule).href));
  const capacityChild = spawn("/bin/bash", ["-e", "-u", "-o", "pipefail", "-s"], {
    cwd: runtimeDirectory, timeout: 20_000,
    env: { PATH: process.env.PATH, AS_SANDBOX: "", OPENCLAW_CONFIG: join(root, "openclaw.json"),
      MODEL: model, KARS_MISSION_CONTRACT: "native-host-fixture", KARS_ROUTER_URL: `http://127.0.0.1:${port}` },
    stdio: ["pipe", "ignore", "pipe"],
  });
  let capacityError = "";
  capacityChild.stderr.on("data", chunk => { capacityError = (capacityError + chunk).slice(-16 * 1024); });
  const capacityExit = new Promise((resolve, reject) => {
    capacityChild.once("error", reject);
    capacityChild.once("close", (code, signal) => resolve({ code, signal }));
  });
  capacityChild.stdin.end(capacityScript);
  assert.deepEqual(await capacityExit, { code: 0, signal: null }, capacityError);
  const preparedConfig = await optionalJson(join(root, "openclaw.json"));
  assert.deepEqual(preparedConfig.models.providers[localProvider].models, [{
    ...config.models.providers[localProvider].models[0], contextWindow: 1_050_000, contextTokens: 1_050_000, maxTokens: 128_000,
  }]);
  const child = spawn(process.execPath, [join(hostRoot, "openclaw.mjs"), "gateway", "run", "--port", String(gatewayPort), "--bind", "loopback"], {
    cwd: runtimeDirectory,
    env: { PATH: process.env.PATH ?? "/usr/bin:/bin", HOME: join(root, "home"), TMPDIR: join(root, "cache"),
      TMP: join(root, "cache"), TEMP: join(root, "cache"), XDG_CACHE_HOME: join(root, "cache"),
      NODE_DISABLE_COMPILE_CACHE: "1", OPENCLAW_STATE_DIR: join(root, "state"), OPENCLAW_CONFIG_PATH: join(root, "openclaw.json"),
      OPENCLAW_GATEWAY_TOKEN: token, OPENCLAW_MODEL: model, KARS_ROUTER_URL: `http://127.0.0.1:${port}`,
      KARS_NATIVE_HOST_RUN_DIR: root, NO_PROXY: "127.0.0.1,localhost",
      HTTP_PROXY: "http://127.0.0.1:8444", HTTPS_PROXY: "http://127.0.0.1:8444",
      NODE_EXTRA_CA_CERTS: join(tlsDirectory, "cert.pem") },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let log = "";
  let spawnError;
  child.stdout.on("data", chunk => { log = (log + chunk).slice(-128 * 1024); });
  child.stderr.on("data", chunk => { log = (log + chunk).slice(-128 * 1024); });
  child.on("error", error => { spawnError = error; });
  const exited = new Promise(resolve => child.once("exit", (code, signal) => resolve({ code, signal })));
  cleanup.push(async () => {
    await stopChild(child, exited);
    await writeFile(join(root, "host-output.log"), log);
    await writeFile(join(root, "http-evidence.json"), JSON.stringify({ requests: boundary.requests,
      errors: boundary.errors, events: boundary.events, authorizations: [...boundary.authorizations.values()], outcomes: [...boundary.outcomes.values()] }, null, 2));
  });
  t.diagnostic(`Persistent genuine-host evidence: ${root}`);
  let receipt;
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    if (spawnError) throw spawnError;
    if (boundary.errors.length) assert.fail(boundary.errors.join("\n"));
    receipt = await optionalJson(join(root, "receipt.json"));
    if (receipt) break;
    assert.equal(child.exitCode, null, `Pinned host exited before receipt:\n${log}`);
    assert.equal(child.signalCode, null, `Pinned host terminated before receipt:\n${log}`);
    await delay(100);
  }
  assert.ok(receipt, `Pinned host did not finish within 120 seconds:\n${log}`);
  assert.equal(receipt.failure, undefined, `Genuine native execution failed: ${JSON.stringify(receipt.failure)}\n${log}`);
  assert.equal(receipt.runtimeVersion, version);
  assert.equal(receipt.registrationMode, "full");
  assert.equal(receipt.result.model, model);
  assert.equal(receipt.result.rounds, 10);
  assert.deepEqual(receipt.result.usage, { promptTokens: 1055, completionTokens: 155, totalTokens: 1210 });
  assert.equal(receipt.result.phase.attemptedToolCalls, 8);
  assert.equal(receipt.result.phase.successfulToolCalls, 5);
  assert.equal(nativeRequests, 1);
  assert.equal(proxyRequests, 1);
  assert.equal(boundary.nativeFetchAttempts, 1);
  assert.equal(boundary.outcomes.get("nativeweb").ok, true);
  assert.equal(receipt.result.artifacts["report.md"], outputText, "Artifact must be reread from the actual output file");
  assert.deepEqual(Object.keys(receipt.result.artifacts).sort(), ["app.py", "kars-router-observations.json", "report.md"]);
  const activity = JSON.parse(receipt.result.artifacts["kars-router-observations.json"]);
  assert.equal(activity.coverage, "native-tool-correlated-only");
  assert.equal(activity.state, "observed");
  assert.equal(activity.correlated_tools, 8);
  assert.equal(activity.responses, undefined);
  assert.deepEqual(activity.rounds, [2, 3, 4, 5, 6, 7, 8, 9], "Neither the rejected draft nor the final text-only round is claimed by tool correlation");
  assert.deepEqual(activity.trace.events.filter(event => event.kind === "tool_proposed").map(event => event.call_id), boundary.plan.map(step => step.id));
  assert.deepEqual(activity.trace.events.filter(event => event.kind === "tool_result").map(event => event.call_id), [...boundary.outcomes.keys()]);
  assert.ok(activity.trace.events.every(event => event.round < 1000 && event.reported_in_round === undefined));
  assert.ok(!JSON.stringify(activity).includes("unrelated_"));
  const live = receipt.progress.filter(update => update.routerObservations).map(update => JSON.parse(update.routerObservations));
  assert.ok(live.length >= 5, "Actual completions must publish Activity before final delivery");
  assert.equal(live[0].correlated_tools, 1);
  assert.deepEqual(live.at(-1), activity);
  assert.equal(await readFile(outputPath, "utf8"), outputText);
  assert.equal(receipt.result.output, "Native host file and governed GET recovery completed.");
  assert.equal(receipt.starts.length, 10, "Count actual native model lifecycle events, not HTTP trace windows");
  assert.equal(new Set(receipt.starts.map(row => row.event.callId)).size, 9, "The pinned host reuses its first call ID on the next attempt");
  assert.equal(receipt.finalizations.length, 2, "The real hook must observe the draft and revised answer");
  const draft = receipt.finalizations[0].messages.filter(message => message.role === "assistant");
  assert.equal(draft.length, 1);
  assert.equal(draft[0].usage.totalTokens, 112);
  assert.equal(receipt.ends.length, 1, "The pinned host suppresses agent_end for the rejected draft");
  assert.ok(receipt.ends[0].event.success);
  const assistants = receipt.ends[0].event.messages.filter(message => message.role === "assistant");
  assert.equal(assistants.length, 9, "The rejected draft is absent from the terminal transcript but its usage is still charged");
  const nativeCalls = assistants.flatMap(message => message.content.filter(block => block.type === "toolCall"));
  assert.deepEqual(nativeCalls.map(call => ({ id: call.id, name: call.name })), boundary.plan.map(({ id, name }) => ({ id, name })));
  assert.equal(assistants.at(-1).stopReason, "stop");
  for (const { context } of boundary.authorizations.values()) {
    assert.ok(receipt.starts.every(({ event, ctx }) => event.runId === context.native_run_id && ctx.sessionId === context.native_session_id));
    assert.equal(receipt.ends[0].ctx.sessionId, context.native_session_id);
  }
  assert.equal(boundary.fetchAttempts, 2);
  const traceCursors = boundary.requests.filter(request => request.path === "/telemetry/trace")
    .map(request => Number(new URLSearchParams(request.query).get("since")));
  assert.equal(traceCursors[0], 40, "Initial correlation uses the router-issued cursor");
  assert.deepEqual(traceCursors.filter(cursor => cursor > 40), boundary.events
    .filter(event => event.kind === "round" && event.round >= 2 && event.round < boundary.plan.length + 1)
    .map(event => event.seq - 1), "Sequential matching advances while retaining each previous proposing round");
  assert.equal(traceCursors.at(-1), 40, "Final Activity export retains the original run cursor");
  const allowed = boundary.plan.filter(step => step.id !== "nativewebdenied").map(step => step.id);
  assert.deepEqual([...boundary.authorizations.keys()], allowed);
  assert.deepEqual([...boundary.outcomes.keys()], allowed);
  const webMessage = boundary.modelRequests[6].messages.find(message => message.tool_call_id === "nativeweb");
  const nativeContent = JSON.parse(webMessage.content);
  assert.equal(nativeContent.truncated, true);
  assert.equal(nativeContent.externalContent.untrusted, true);
  assert.equal(nativeContent.externalContent.wrapped, true);
  assert.ok(nativeContent.spill.path.startsWith(join(root, "cache") + "/"));
  const spilled = await readFile(nativeContent.spill.path, "utf8");
  assert.ok(spilled.includes(readableMarker));
  assert.ok(!spilled.includes("RAW_SCRIPT_SENTINEL"));
  const extracted = spilled.match(/Source: Web Fetch\n---\n([\s\S]*)\n<<<END_EXTERNAL_UNTRUSTED_CONTENT/);
  assert.ok(extracted, "Native spill must retain the untrusted-content wrapper");
  assert.equal(extracted[1].length, nativeContent.spill.chars);
  assert.ok(spilled.length > 100000, "Native spill must preserve the full extracted page outside model context");
  assert.equal(boundary.outcomes.get("nativeread").ok, true);
  assert.equal(boundary.outcomes.get("nativewrite").ok, true);
  assert.deepEqual(boundary.outcomes.get("nativeread").result_summary,
    { kind: "file_read", bytes: Buffer.byteLength(inputText), returned_bytes: Buffer.byteLength(inputText), truncated: false });
  assert.equal(boundary.outcomes.get("nativefetchfailed").ok, false);
  assert.deepEqual(boundary.outcomes.get("nativefetchfailed").result_summary, { kind: "failure", reason: "tool_execution_failed" });
  assert.equal(boundary.outcomes.get("nativefetchrecovered").ok, true);
  assert.equal(boundary.outcomes.get("nativefetchrecovered").result_summary, undefined);
  assert.deepEqual(boundary.outcomes.get("nativewrite").result_summary,
    { kind: "file_write", bytes: Buffer.byteLength(outputText), artifact_name: "report.md" });
  assert.equal(boundary.errors.length, 0);

  await t.test("registered file_write copies retained input bytes exactly and rejects a wrong sha256", async () => {
    const expected = Buffer.from(copyInput.content, "utf8");
    const exported = Buffer.from(receipt.result.artifacts[copyInput.name], "utf8");
    const persisted = await readFile(copyPath);
    assert.deepEqual(persisted, expected, "Native writer must preserve BOM, Unicode and mixed newlines");
    assert.deepEqual(exported, expected, "Collected artifact must preserve the retained input bytes");
    for (const bytes of [persisted, exported]) {
      assert.equal(`sha256:${createHash("sha256").update(bytes).digest("hex")}`, copyInput.sha256);
    }
    for (const id of ["nativecopywrong", "nativecopy"]) {
      const args = boundary.authorizations.get(id).context.arguments;
      assert.equal(args.source_path, `/sandbox/.kars-inputs/${copyInput.name}`);
      assert.equal(Object.hasOwn(args, "content"), false, "Copy must dispatch without model-transcribed content");
    }
    assert.equal(boundary.outcomes.get("nativecopywrong").ok, false);
    assert.deepEqual(boundary.outcomes.get("nativecopywrong").result_summary,
      { kind: "failure", reason: "tool_execution_failed" });
    assert.equal(boundary.outcomes.get("nativecopy").ok, true);
    assert.deepEqual(boundary.outcomes.get("nativecopy").result_summary,
      { kind: "file_write", bytes: expected.length, artifact_name: copyInput.name });
  });
});
