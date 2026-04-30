// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Phase 2 / S15.b: port + WS health helpers extracted from
// mesh.ts. Public surface preserved (re-exported from mesh.ts).

import chalk from "chalk";
import * as crypto from "node:crypto";
import * as http from "node:http";
import { execa } from "execa";
import { checkLine } from "../../stepper.js";

/** Kill all processes listening on the given ports (prevents duplicate port-forwards). */
export async function killProcessesOnPorts(ports: number[]): Promise<void> {
  for (const port of ports) {
    try {
      // -sTCP:LISTEN — only kill LISTENERS, not processes with active connections through the port
      const { stdout } = await execa("lsof", ["-ti", `:${port}`, "-sTCP:LISTEN"], { stdio: "pipe", reject: false });
      const pidsOnPort = stdout.trim().split("\n").filter(Boolean);
      for (const p of pidsOnPort) {
        try { process.kill(parseInt(p, 10), "SIGTERM"); } catch { /* already dead */ }
      }
      if (pidsOnPort.length > 0) {
        console.log(chalk.dim(`  · Cleared ${pidsOnPort.length} listener(s) on port ${port}`));
      }
    } catch { /* no process on port */ }
  }
  if (ports.length > 0) await new Promise(r => setTimeout(r, 500));
}

/** Post-start cleanup: kill any listener on our ports that isn't one of our spawned PIDs. */
export async function killStaleListeners(portPidMap: Array<{ port: number; pid: number }>): Promise<void> {
  const ownPids = new Set(portPidMap.map(pp => pp.pid));
  const ports = [...new Set(portPidMap.map(pp => pp.port))];
  for (const port of ports) {
    try {
      const { stdout } = await execa("lsof", ["-ti", `:${port}`, "-sTCP:LISTEN"], { stdio: "pipe", reject: false });
      const listeners = stdout.trim().split("\n").filter(Boolean).map(Number);
      for (const pid of listeners) {
        if (!ownPids.has(pid)) {
          try { process.kill(pid, "SIGKILL"); } catch { /* already dead */ }
          console.log(chalk.dim(`  · Killed stale listener on port ${port} (PID ${pid})`));
        }
      }
    } catch { /* ignore */ }
  }
}

/** Find ports with multiple listeners (indicates stale port-forwards). */
export async function findDuplicateListeners(ports: number[]): Promise<Array<{ port: number; count: number; pids: number[] }>> {
  const results: Array<{ port: number; count: number; pids: number[] }> = [];
  for (const port of ports) {
    try {
      const { stdout } = await execa("lsof", ["-ti", `:${port}`, "-sTCP:LISTEN"], { stdio: "pipe", reject: false });
      const pids = stdout.trim().split("\n").filter(Boolean).map(Number);
      if (pids.length > 1) {
        results.push({ port, count: pids.length, pids });
      }
    } catch { /* ignore */ }
  }
  return results;
}

/** Check registry health via HTTP /v1/health endpoint. */
export async function checkRegistryHealth(port: number): Promise<boolean> {
  try {
    const resp = await fetch(`http://localhost:${port}/v1/health`, {
      signal: AbortSignal.timeout(5000),
    });
    if (resp.ok) {
      const body = await resp.json() as Record<string, unknown>;
      checkLine(true, `Registry healthy (${body.agents_registered ?? 0} agents, ${body.agents_online ?? 0} online)`);
      return true;
    }
    checkLine(false, `Registry returned HTTP ${resp.status}`);
    return false;
  } catch (e: unknown) {
    checkLine(false, `Registry not reachable: ${e instanceof Error ? e.message : String(e)}`);
    return false;
  }
}

/** Check relay health via WebSocket upgrade (not just TCP connect). */
export async function checkRelayHealth(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const req = http.request({
      hostname: "127.0.0.1",
      port,
      path: "/",
      method: "GET",
      headers: {
        "Upgrade": "websocket",
        "Connection": "Upgrade",
        "Sec-WebSocket-Key": crypto.randomBytes(16).toString("base64"),
        "Sec-WebSocket-Version": "13",
      },
      timeout: 5000,
    });

    req.on("upgrade", (_res, socket) => {
      checkLine(true, `Relay healthy (WebSocket upgrade on localhost:${port})`);
      socket.destroy();
      resolve(true);
    });

    req.on("response", (res) => {
      // Got an HTTP response instead of upgrade — relay is serving but not WS
      if (res.statusCode === 101) {
        checkLine(true, `Relay healthy (localhost:${port})`);
        resolve(true);
      } else {
        checkLine(false, `Relay returned HTTP ${res.statusCode} (expected WebSocket upgrade)`);
        resolve(false);
      }
    });

    req.on("error", (e) => {
      checkLine(false, `Relay not reachable: ${e.message}`);
      resolve(false);
    });

    req.on("timeout", () => {
      checkLine(false, `Relay timeout on localhost:${port}`);
      req.destroy();
      resolve(false);
    });

    req.end();
  });
}
