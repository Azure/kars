// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Phase 2 / S15 hotspot-pass3: extracted helpers + state for
// `kars handoff`. The command is intentionally a single
// long-lived action handler — its many helpers all share one piece
// of mutable state (the AKS port-forward child process + its local
// port). To keep handoff.ts under the §15 hotspot cap, this module
// owns the helpers as a factory-returned bundle; handoff.ts only
// keeps the orchestration body.
//
// No behavioral change vs. the prior in-line closures — the closure
// captures (containerName / targetNs / aksPfPort / aksPfProc) just
// migrate from action-scope to factory-scope.

import chalk from "chalk";
import type { ChildProcess } from "node:child_process";
import { banner, kvLine } from "../../stepper.js";

// Shared tar command for workspace + config collection.
// Captures: workspace, gateway config, cron jobs, governance policies, agent state.
// Excludes: compiled extensions (regenerable), node_modules, git, python cache.
export const WORKSPACE_TAR_CMD =
  "tar czf - -C /sandbox " +
  "--exclude='.openclaw/extensions/*/dist' --exclude='.openclaw/extensions/*/node_modules' " +
  "--exclude='node_modules' --exclude='.git' " +
  "--exclude='*.pyc' --exclude='__pycache__' " +
  ".openclaw/workspace .openclaw/openclaw.json .openclaw/cron " +
  ".openclaw/policies .openclaw/agents 2>/dev/null | base64 -w0";

export interface HandoffHelpers {
  readonly execa: typeof import("execa").execa;
  readonly containerName: string;
  readonly targetNs: string;
  readonly aksPfPort: number;
  readonly WORKSPACE_TAR_CMD: string;
  routerExec(
    method: string,
    path: string,
    body?: unknown,
    extraHeaders?: Record<string, string>,
  ): Promise<{ status: number; body: any }>;
  getAdminToken(): Promise<string | undefined>;
  aksPortForwardStart(): Promise<void>;
  aksPortForwardStop(): void;
  aksRouterExec(
    method: string,
    path: string,
    body?: unknown,
    extraHeaders?: Record<string, string>,
  ): Promise<{ status: number; body: any }>;
  getAksAdminToken(): Promise<string | undefined>;
  wakeDormantDocker(): Promise<{ ready: boolean; error?: string }>;
  readAksCrdSpec(): Promise<{
    model: string;
    learnEgress: boolean;
    isolation: string;
    trustThreshold: number;
  }>;
  rehydrateCredentials(): Promise<string[]>;
}

export async function createHandoffHelpers(name: string): Promise<HandoffHelpers> {
  const { execa } = await import("execa");
  const containerName = `kars-${name}`;
  const targetNs = `kars-${name}`;
  const aksPfPort = 18445; // temp local port for source AKS pod
  let aksPfProc: ChildProcess | undefined;

  async function routerExec(
    method: string,
    path: string,
    body?: unknown,
    extraHeaders?: Record<string, string>,
  ): Promise<{ status: number; body: any }> {
    const curlArgs = [
      "exec", containerName,
      "curl", "-sf", "--max-time", "30",
      "-X", method,
      "-H", "Content-Type: application/json",
    ];
    if (extraHeaders) {
      for (const [k, v] of Object.entries(extraHeaders)) {
        curlArgs.push("-H", `${k}: ${v}`);
      }
    }
    if (body) {
      curlArgs.push("-d", JSON.stringify(body));
    }
    curlArgs.push("-w", "\n%{http_code}");
    curlArgs.push(`http://127.0.0.1:8443${path}`);

    const { stdout } = await execa("docker", curlArgs, { stdio: "pipe" });
    const lines = stdout.trimEnd().split("\n");
    const statusCode = parseInt(lines[lines.length - 1], 10);
    const responseBody = lines.slice(0, -1).join("\n");
    try {
      return { status: statusCode, body: JSON.parse(responseBody) };
    } catch {
      return { status: statusCode, body: { raw: responseBody } };
    }
  }

  async function getAdminToken(): Promise<string | undefined> {
    try {
      const { stdout } = await execa("docker", [
        "exec", containerName,
        "printenv", "ADMIN_TOKEN",
      ], { stdio: "pipe" });
      return stdout.trim() || undefined;
    } catch {
      try {
        const { stdout } = await execa("docker", [
          "exec", containerName,
          "cat", "/run/secrets/admin-token",
        ], { stdio: "pipe" });
        return stdout.trim() || undefined;
      } catch {
        // Fallback: entrypoint saves the token to /tmp/.agt-admin-token
        try {
          const { stdout } = await execa("docker", [
            "exec", containerName,
            "cat", "/tmp/.agt-admin-token",
          ], { stdio: "pipe" });
          return stdout.trim() || undefined;
        } catch {
          return undefined;
        }
      }
    }
  }

  async function aksPortForwardStart(): Promise<void> {
    if (aksPfProc) return;
    aksPfProc = execa("kubectl", [
      "port-forward", "-n", targetNs,
      `svc/${name}`, `${aksPfPort}:8443`,
    ], { stdio: "pipe", reject: false }) as any;
    const http = await import("node:http");
    for (let i = 0; i < 15; i++) {
      await new Promise((r) => setTimeout(r, 1000));
      try {
        const ok: boolean = await new Promise((resolve) => {
          const req = http.get(
            `http://127.0.0.1:${aksPfPort}/readyz`,
            { timeout: 2000 },
            (res) => {
              let data = "";
              res.on("data", (c: Buffer) => {
                data += c.toString();
              });
              res.on("end", () => resolve(res.statusCode === 200));
            },
          );
          req.on("error", () => resolve(false));
          req.on("timeout", () => {
            req.destroy();
            resolve(false);
          });
        });
        if (ok) return;
      } catch {
        /* retry */
      }
    }
    throw new Error("AKS port-forward not ready after 15s");
  }

  function aksPortForwardStop(): void {
    if (aksPfProc) {
      try {
        (aksPfProc as any).kill();
      } catch {
        /* ignore */
      }
      aksPfProc = undefined;
    }
  }

  async function aksRouterExec(
    method: string,
    path: string,
    body?: unknown,
    extraHeaders?: Record<string, string>,
  ): Promise<{ status: number; body: any }> {
    const http = await import("node:http");
    const payload = body ? JSON.stringify(body) : undefined;
    return new Promise((resolve, reject) => {
      const opts: any = {
        hostname: "127.0.0.1",
        port: aksPfPort,
        path,
        method,
        headers: {
          "Content-Type": "application/json",
          ...(extraHeaders || {}),
          ...(payload ? { "Content-Length": Buffer.byteLength(payload) } : {}),
        },
        timeout: 30000,
      };
      const req = http.request(opts, (res: any) => {
        let data = "";
        res.on("data", (c: Buffer) => {
          data += c.toString();
        });
        res.on("end", () => {
          try {
            resolve({ status: res.statusCode, body: JSON.parse(data) });
          } catch {
            resolve({ status: res.statusCode, body: { raw: data } });
          }
        });
      });
      req.on("error", reject);
      req.on("timeout", () => {
        req.destroy();
        reject(new Error("timeout"));
      });
      if (payload) req.write(payload);
      req.end();
    });
  }

  async function getAksAdminToken(): Promise<string | undefined> {
    // On AKS, the controller mounts the admin token at
    // /etc/kars/secrets/admin-token. Read it via the openclaw container — the
    // inference-router image is distroless (no `cat`); openclaw shares the same
    // mount and has a shell. (Falls back to the K8s secret below.)
    try {
      const { stdout } = await execa("kubectl", [
        "exec", "-n", targetNs,
        `deploy/${name}`, "-c", "openclaw", "--",
        "cat", "/etc/kars/secrets/admin-token",
      ], { stdio: "pipe", reject: false });
      if (stdout.trim()) return stdout.trim();
    } catch {
      /* fallback */
    }
    // Fallback: read from K8s secret directly
    try {
      const { stdout } = await execa("kubectl", [
        "get", "secret", "router-admin-token", "-n", targetNs,
        "-o", "jsonpath={.data.token}",
      ], { stdio: "pipe", reject: false });
      if (stdout.trim()) {
        return Buffer.from(stdout.trim(), "base64").toString("utf8").trim();
      }
    } catch {
      /* no secret */
    }
    return undefined;
  }

  async function wakeDormantDocker(): Promise<{ ready: boolean; error?: string }> {
    let containerState: string | undefined;
    try {
      const { stdout } = await execa("docker", [
        "inspect", "-f", "{{.State.Status}}", containerName,
      ], { stdio: "pipe" });
      containerState = stdout.trim();
    } catch {
      return {
        ready: false,
        error: `Container '${containerName}' not found. Run 'kars dev --name ${name}' first.`,
      };
    }

    if (containerState === "exited" || containerState === "created") {
      try {
        await execa("docker", ["start", containerName], { stdio: "pipe" });
      } catch (e: any) {
        return { ready: false, error: `Failed to start container: ${e.message}` };
      }
    } else if (containerState !== "running") {
      return { ready: false, error: `Container in unexpected state: ${containerState}` };
    }

    for (let i = 0; i < 30; i++) {
      try {
        await execa("docker", [
          "exec", containerName, "sh", "-c",
          "wget -qO- --timeout=2 http://127.0.0.1:8443/readyz 2>/dev/null || curl -sf --max-time 2 http://127.0.0.1:8443/readyz 2>/dev/null",
        ], { stdio: "pipe" });
        return { ready: true };
      } catch {
        /* not ready yet */
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
    return { ready: false, error: "Local router not healthy after 30s" };
  }

  async function readAksCrdSpec(): Promise<{
    model: string;
    learnEgress: boolean;
    isolation: string;
    trustThreshold: number;
  }> {
    const defaults = {
      model: "gpt-5.4",
      learnEgress: true,
      isolation: "enhanced",
      trustThreshold: 500,
    };
    try {
      const { stdout } = await execa("kubectl", [
        "get", "karssandbox", name, "-n", "kars-system",
        "-o", "json",
      ], { stdio: "pipe" });
      const obj = JSON.parse(stdout);
      const spec = obj.spec ?? {};
      const annotations: Record<string, string> = obj.metadata?.annotations ?? {};

      // Post-S10/S13: model lives on the InferencePolicy CR (referenced by
      // spec.inferenceRef.name), not on the sandbox spec. Resolve it.
      let model: string | undefined;
      const inferenceRefName = spec.inferenceRef?.name;
      if (typeof inferenceRefName === "string" && inferenceRefName.length > 0) {
        try {
          const { stdout: ipStdout } = await execa("kubectl", [
            "get", "inferencepolicy", inferenceRefName, "-n", "kars-system",
            "-o", "jsonpath={.spec.modelPreference.primary.deployment}",
          ], { stdio: "pipe" });
          const dep = ipStdout.trim();
          if (dep.length > 0) model = dep;
        } catch {
          /* ip not found / not Ready — fall through */
        }
      }
      // Final fallback chain: annotation, legacy spec.inference (older
      // CRs created before S10.A1), legacy openclaw.config, defaults.
      model =
        model ||
        annotations["kars.azure.com/model"] ||
        spec.inference?.model ||
        spec.runtime?.openclaw?.config?.agent?.model?.replace("azure/", "") ||
        defaults.model;

      return {
        model: model || defaults.model,
        learnEgress: (() => {
          const mode = spec.networkPolicy?.egressMode;
          return typeof mode === "string"
            ? mode === "Learn"
            : defaults.learnEgress;
        })(),
        isolation: spec.sandbox?.isolation || defaults.isolation,
        trustThreshold: spec.governance?.trustThreshold || defaults.trustThreshold,
      };
    } catch {
      return defaults;
    }
  }

  async function rehydrateCredentials(): Promise<string[]> {
    const injected: string[] = [];
    try {
      const { stdout } = await execa("kubectl", [
        "get", "secret", `${name}-credentials`, "-n", targetNs,
        "-o", "json",
      ], { stdio: "pipe" });
      const secret = JSON.parse(stdout);
      const data = secret.data || {};
      for (const [key, b64] of Object.entries(data)) {
        const val = Buffer.from(b64 as string, "base64").toString("utf8");
        if (val) {
          injected.push(key);
        }
      }
      if (injected.length > 0) {
        const envLines = Object.entries(data)
          .map(
            ([k, b64]) =>
              `${k}=${Buffer.from(b64 as string, "base64").toString("utf8")}`,
          )
          .join("\n");
        await execa("docker", [
          "exec", containerName, "sh", "-c",
          `cat > /tmp/.handoff-credentials << 'CRED_EOF'\n${envLines}\nCRED_EOF`,
        ], { stdio: "pipe" });
      }
    } catch {
      // K8s secret not available or no kubectl — fall back silently
    }
    return injected;
  }

  return {
    execa,
    containerName,
    targetNs,
    aksPfPort,
    WORKSPACE_TAR_CMD,
    routerExec,
    getAdminToken,
    aksPortForwardStart,
    aksPortForwardStop,
    aksRouterExec,
    getAksAdminToken,
    wakeDormantDocker,
    readAksCrdSpec,
    rehydrateCredentials,
  };
}

/** Renders the `--status` branch. */
export async function runStatus(name: string, h: HandoffHelpers): Promise<void> {
  try {
    const adminToken = await h.getAdminToken();
    const headers: Record<string, string> = {};
    if (adminToken) headers["Authorization"] = `Bearer ${adminToken}`;

    const resp = await h.routerExec("GET", "/agt/handoff/status", undefined, headers);
    const s = resp.body;

    banner("kars · Handoff Status", name);

    kvLine("Phase", s.phase || "idle");
    kvLine("Direction", s.direction || "—");
    kvLine("Registry mode", s.registry_mode || "unknown");
    kvLine(
      "Handoff available",
      s.handoff_available
        ? chalk.green("yes")
        : chalk.yellow("no (requires --global-registry)"),
    );
    if (s.predecessor_amid) kvLine("Predecessor", s.predecessor_amid);
    if (s.successor_amid) kvLine("Successor", s.successor_amid);
    if (s.snapshot_size_bytes)
      kvLine("Snapshot size", `${(s.snapshot_size_bytes / 1024).toFixed(1)} KB`);
    if (s.draining) kvLine("Draining", `${s.drain_duration_secs || 0}s`);
    if (s.error) kvLine("Error", chalk.red(s.error));

    console.log();
  } catch (e: any) {
    console.log(chalk.red(`\n  Could not reach sandbox '${name}': ${e.message}\n`));
    process.exit(1);
  }
}

/** Renders the `--abort` branch. */
export async function runAbort(h: HandoffHelpers): Promise<void> {
  try {
    const adminToken = await h.getAdminToken();
    if (!adminToken) {
      console.log(chalk.red("\n  Cannot abort: admin token not found.\n"));
      process.exit(1);
    }

    const statusResp = await h.routerExec("GET", "/agt/handoff/status", undefined, {
      Authorization: `Bearer ${adminToken}`,
    });

    if (!statusResp.body.handoff_token_active) {
      console.log(
        chalk.yellow(`\n  No active handoff to abort (phase: ${statusResp.body.phase}).\n`),
      );
      return;
    }

    console.log(
      chalk.yellow(`\n  Aborting handoff (current phase: ${statusResp.body.phase})...`),
    );

    // The abort endpoint requires the handoff token, which only the
    // initiating CLI process has. If we don't have it, we can't abort
    // from a different terminal. Show guidance instead.
    console.log(
      chalk.dim("  Note: abort must be called from the terminal that initiated the handoff."),
    );
    console.log(chalk.dim("  The handoff token is held in that process's memory.\n"));
  } catch (e: any) {
    console.log(chalk.red(`\n  Abort failed: ${e.message}\n`));
    process.exit(1);
  }
}
