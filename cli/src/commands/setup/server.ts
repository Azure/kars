// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import { createServer, type IncomingMessage } from "node:http";
import { SetupBackend } from "./backend.js";
import { SetupError } from "./types.js";
import { renderSetupPage } from "./page.js";

export type SetupService = Pick<SetupBackend, "initialize" | "state" | "preflight" | "install"> & Partial<Pick<SetupBackend, "prerequisites" | "startCopilot" | "cancelCopilot" | "refreshCopilotModels" | "cleanup" | "dispose">>;
const equal = (left: string, right: string) => left.length === right.length
  && timingSafeEqual(Buffer.from(left), Buffer.from(right));
const token = () => randomBytes(32).toString("hex");

async function body(request: IncomingMessage): Promise<unknown> {
  if (request.headers["content-type"] !== "application/json") throw new SetupError("JSON is required");
  let size = 0;
  const chunks: Buffer[] = [];
  for await (const chunk of request) {
    size += chunk.length;
    if (size > 16_384) throw new SetupError("Request is too large");
    chunks.push(chunk);
  }
  try { return JSON.parse(Buffer.concat(chunks).toString("utf8")); }
  catch { throw new SetupError("Invalid JSON"); }
}

/** One private, same-origin loopback session; no arbitrary command or file API. */
export async function startSetupServer(backend: SetupService = new SetupBackend(), idleMs = 30 * 60_000) {
  await backend.initialize();
  const page = renderSetupPage();
  const hash = (content: string) => `'sha256-${createHash("sha256").update(content).digest("base64")}'`;
  const scripts = [...page.matchAll(/<script>([\s\S]*?)<\/script>/gi)].map(match => hash(match[1])).join(" ");
  const styles = [...page.matchAll(/<style>([\s\S]*?)<\/style>/gi)].map(match => hash(match[1])).join(" ");
  let bootstrap = token(), session = "", csrf = "", origin = "", lastActivity = Date.now(), stopped = false;
  const created = Date.now(), cookieName = `kars_setup_${token().slice(0, 12)}`;
  let settle: () => void;
  const closed = new Promise<void>(resolve => { settle = resolve; });
  const server = createServer(async (request, response) => {
    response.setHeader("Cache-Control", "no-store");
    response.setHeader("Referrer-Policy", "no-referrer");
    response.setHeader("X-Content-Type-Options", "nosniff");
    response.setHeader("X-Frame-Options", "DENY");
    response.setHeader("Content-Security-Policy", `default-src 'none'; script-src ${scripts}; style-src ${styles}; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; object-src 'none'`);
    const json = (status: number, value: unknown) => {
      response.writeHead(status, { "Content-Type": "application/json" });
      response.end(JSON.stringify(value));
    };
    try {
      // External links may open the public landing page, never a setup API or embedded document.
      const landingNavigation = request.method === "GET" && request.url === "/"
        && request.headers["sec-fetch-mode"] === "navigate" && request.headers["sec-fetch-dest"] === "document";
      if (stopped || request.headers.host !== new URL(origin).host
        || (request.headers["sec-fetch-site"] === "cross-site" && !landingNavigation)) { json(403, { error: "Local origin required" }); return; }
      if (request.method === "POST" && request.headers.origin !== origin) { json(403, { error: "Local origin required" }); return; }
      if (request.method === "GET" && request.url === "/") {
        response.writeHead(200, { "Content-Type": "text/html; charset=utf-8" }); response.end(page); return;
      }
      if (request.method === "POST" && request.url === "/api/session") {
        const input = await body(request) as { token?: unknown };
        if (!bootstrap || Date.now() - created > 10 * 60_000 || typeof input?.token !== "string" || !/^[a-f0-9]{64}$/.test(input.token) || !equal(input.token, bootstrap)) { json(401, { error: "Private setup link expired or already used" }); return; }
        bootstrap = ""; session = token(); csrf = token(); lastActivity = Date.now();
        response.setHeader("Set-Cookie", `${cookieName}=${session}; HttpOnly; SameSite=Strict; Path=/`);
        json(200, { csrfToken: csrf }); return;
      }
      const cookie = request.headers.cookie?.split(";").map(part => part.trim()).find(part => part.startsWith(`${cookieName}=`))?.slice(cookieName.length + 1) ?? "";
      if (!session || !/^[a-f0-9]{64}$/.test(cookie) || !equal(cookie, session) || Date.now() - lastActivity > idleMs) { json(401, { error: "Use the private setup link from the terminal" }); return; }
      lastActivity = Date.now();
      if (request.method === "GET" && request.url === "/api/state") { json(200, { ...backend.state, csrfToken: csrf }); return; }
      if (request.method !== "POST") { json(404, { error: "Unknown setup endpoint" }); return; }
      const header = request.headers["x-csrf-token"];
      if (typeof header !== "string" || !/^[a-f0-9]{64}$/.test(header) || !equal(header, csrf)) { json(403, { error: "Setup session token required" }); return; }
      if (request.url === "/api/copilot/start" || request.url === "/api/copilot/cancel" || request.url === "/api/copilot/models") {
        await body(request);
        const action = request.url.endsWith("/start") ? backend.startCopilot : request.url.endsWith("/models") ? backend.refreshCopilotModels : backend.cancelCopilot;
        if (!action) throw new SetupError("GitHub sign-in is unavailable in this installer");
        json(200, action.call(backend)); return;
      }
      if (request.url === "/api/prerequisites") {
        const input = await body(request);
        if (!backend.prerequisites) throw new SetupError("Prerequisite checks are unavailable in this installer");
        json(200, await backend.prerequisites(input)); return;
      }
      if (request.url === "/api/preflight") {
        const operation = backend.preflight(await body(request));
        await Promise.race([operation, Promise.resolve()]);
        void operation.catch(error => {
          backend.state.phase = "blocked"; backend.state.busy = false;
          backend.state.message = error instanceof SetupError ? error.message : "Preflight failed. No installation was started; command output and credentials are not displayed.";
        });
        json(backend.state.busy ? 202 : 200, backend.state); return;
      }
      if (request.url === "/api/install") {
        const input = await body(request);
        // Admission must synchronously bind and consume approval before returning state.
        const operation = backend.install(input);
        await Promise.race([operation, Promise.resolve()]);
        void operation.catch(() => { backend.state.phase = "failed"; backend.state.busy = false; backend.state.message = "Installation failed; inspect retained private receipts."; });
        json(202, backend.state); return;
      }
      if (request.url === "/api/cleanup") {
        if (!backend.cleanup) throw new SetupError("Owned cleanup is unavailable in this installer");
        const operation = backend.cleanup(await body(request));
        await Promise.race([operation, Promise.resolve()]);
        void operation.catch(() => {
          backend.state.busy = false;
          backend.state.cleanup = { status: "failed", detail: "Cleanup failed; inspect retained ownership receipts before retrying." };
        });
        json(backend.state.busy ? 202 : 200, backend.state); return;
      }
      if (request.url === "/api/shutdown") {
        await body(request);
        if (backend.state.busy) throw new SetupError("Installation is running; keep the installer open");
        response.setHeader("Set-Cookie", `${cookieName}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0`);
        json(200, { closed: true }); stop(); return;
      }
      json(404, { error: "Unknown setup endpoint" });
    } catch (error) {
      json(400, { error: error instanceof SetupError ? error.message : "Setup request failed. Credentials and command output are not displayed." });
    }
  });
  server.requestTimeout = 15_000; server.headersTimeout = 10_000; server.maxHeadersCount = 32;
  server.on("connection", socket => { socket.setTimeout(5 * 60_000, () => socket.destroy()); });
  const stop = () => {
    if (stopped) return;
    stopped = true; bootstrap = ""; session = ""; csrf = ""; clearInterval(expiry); backend.dispose?.();
    server.close(() => settle()); server.closeIdleConnections();
  };
  const expiry = setInterval(() => { if (!backend.state.busy && Date.now() - lastActivity > idleMs) stop(); }, Math.min(idleMs, 30_000));
  expiry.unref();
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject); server.listen(0, "127.0.0.1", () => { server.removeListener("error", reject); resolve(); });
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Loopback server address unavailable");
  origin = `http://127.0.0.1:${address.port}`;
  return { url: `${origin}/#token=${bootstrap}`, origin, close: stop, closed };
}

export async function launchSetupBrowser(failedCluster?: string): Promise<void> {
  const backend = new SetupBackend();
  if (failedCluster) await backend.recoverFailedAttempt(failedCluster);
  const setup = await startSetupServer(backend);
  const guidance = failedCluster
    ? "Recovery only: no installation will be replayed. Cleanup starts only after you confirm it in the browser."
    : "No installation starts until you approve it in the browser.";
  console.log(`\nPrivate Kars setup link (do not share):\n${setup.url}\n\nKeep this terminal open. ${guidance}`);
  const { openBrowser } = await import("../up/headlamp_stack.js");
  try { await openBrowser(setup.url); } catch { console.log("Open the private link above in your browser."); }
  const stop = () => setup.close();
  process.once("SIGINT", stop); process.once("SIGTERM", stop);
  try { await setup.closed; } finally { process.removeListener("SIGINT", stop); process.removeListener("SIGTERM", stop); }
}
