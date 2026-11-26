// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { SetupBackend, parseSetupDraft, validateProvider, verifySetupProvider, setupCleanupCommand, executeSetup, type SetupDependencies } from "./backend.js";
import { execa } from "execa";
import { execFileSync } from "node:child_process";
import { SetupError } from "./types.js";
import { SetupCopilotLogin } from "./copilot.js";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { randomUUID, X509Certificate } from "node:crypto";
import { createServer } from "node:net";
import { assertPortAvailable } from "../dev/kind-beta.js";
import { betaDirectory, readBetaState, writeBetaState } from "../dev/kind-beta-state.js";

vi.mock("execa", () => ({ execa: vi.fn() }));

const draft = () => ({ target: "kind", clusterName: "concierge-unit-fixture", context: "", bridge: true,
  provider: { kind: "azure-openai" as const, endpoint: "https://fixture.openai.azure.com", model: "test-model", apiKey: "fixture-key" },
  admin: { email: "fixture@example.com", password: "Unit-fixture-password-1" }, consent: { target: false, hostTrust: false } });
const models = async () => [{ id: "gpt-5", name: "GPT 5" }, { id: "test-model", name: "Test model" }];
function fixture(overrides: Partial<SetupDependencies> = {}, copilot?: SetupCopilotLogin) {
  const calls: string[][] = [];
  const dependencies: SetupDependencies = {
    execute: async (tool, args) => {
      calls.push([tool, ...args]);
      if (tool === "which") return { stdout: `/fixture/${args[0]}` };
      if (args[0] === "--version") return { stdout: "kind v0.31.0" };
      if (args.join(" ") === "version --short") return { stdout: "v4.1.3" };
      if (args.includes("connection")) return { stdout: JSON.stringify([{ Default: true, URI: "unix:///fixture/podman.sock" }]) };
      if (args[0] === "ps") return { stdout: "[]" };
      if (args[0] === "info") return { stdout: args.includes("--format") ? "amd64" : "{}" };
      if (args[0] === "config") return { stdout: JSON.stringify({ "current-context": "fixture-aks", contexts: [{ name: "fixture-aks", context: { cluster: "aks" } }], clusters: [{ name: "aks", cluster: { server: "https://fixture.azmk8s.io" } }] }) };
      if (args.includes("namespace")) return { stdout: JSON.stringify({ metadata: { uid: "workspace-uid" } }) };
      if (args.includes("deployment")) return { stdout: JSON.stringify({ metadata: { uid: "controller-uid", resourceVersion: "1",
        labels: { "app.kubernetes.io/managed-by": "Helm" }, annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" } } }) };
      if (tool === "helm" && args.includes("list")) return { stdout: JSON.stringify([{ name: "kars", namespace: "kars-system", status: "deployed", chart: "kars-0.2.0-beta.6" }]) };
      if (tool === "helm" && args.includes("values")) return { stdout: JSON.stringify({ controller: { inferenceDefaults: { provider: "openai", model: "existing-model" } }, apiKey: "private-existing-key" }) };
      return { stdout: "" };
    },
    authorizeTerminal: vi.fn(async () => {}), assets: vi.fn(), architecture: async () => "amd64", hostSupport: vi.fn(), port: vi.fn(async () => {}), verifyProvider: vi.fn(async () => {}),
    install: vi.fn(async () => { throw new Error("fixture install deliberately fails"); }), root: "/nonexistent-kars-test-fixture",
    ...overrides,
  };
  return { backend: new SetupBackend(dependencies, copilot), calls, dependencies };
}

describe("setup backend", () => {
  it("retains safe hook diagnoses through owned cleanup and displays them when reopening a failure", async () => {
    const root = mkdtempSync(path.join(tmpdir(), "kars-hook-diagnosis-"));
    const directory = betaDirectory(draft().clusterName, root);
    const { dependencies } = fixture({ root, install: async (_cluster, _tools, _hooks, input) => {
      mkdirSync(directory, { mode: 0o700 });
      writeBetaState(directory, { version: 1, owner: randomUUID(), cluster: draft().clusterName, phase: "failed",
        runtime: "podman", endpoint: "unix:///owned.sock", nodeId: "a".repeat(64) });
      input?.progress?.("core");
      throw new Error("private-install-error");
    } });
    const execute = dependencies.execute;
    dependencies.execute = async (tool, args, options) => {
      if (tool === "podman" && args[0] === "ps" && args.includes("{{.ID}}")) return { stdout: "" };
      if (!args.includes("nodes,pods,jobs,deployments")) return execute(tool, args, options);
      return { stdout: JSON.stringify({ items: [{ kind: "Pod",
        metadata: { name: "kars-sre-install-unit-pod", namespace: "kars-system" },
        spec: { containers: [{ name: "sre-enrollment" }] }, status: { phase: "Failed",
          containerStatuses: [{ name: "sre-enrollment", state: { terminated: { exitCode: 1,
            message: "private-output\nKARS_CORE_HOOK_FAILURE ControllerChanged" } } }] } }] }) };
    };
    const backend = new SetupBackend(dependencies), reopened = new SetupBackend(dependencies);
    try {
      await backend.preflight(draft());
      await backend.install({ ...draft(), consent: { target: true, hostTrust: true } });
      expect(readBetaState(directory)?.failure?.coreHookFailures).toEqual(["ControllerChanged"]);
      expect(backend.state.checks).toContainEqual(expect.objectContaining({ id: "core-hook-ControllerChanged", status: "failed" }));
      expect(readFileSync(path.join(directory, "managed-local.json"), "utf8")).not.toContain("private-output");
      await reopened.recoverFailedAttempt(draft().clusterName);
      expect(reopened.state.checks).toContainEqual(expect.objectContaining({ diagnosticCode: "ControllerChanged" }));
      await backend.cleanup({ clusterName: draft().clusterName, confirm: true });
      expect(readBetaState(directory)).toMatchObject({ phase: "deleted", failure: { coreHookFailures: ["ControllerChanged"] } });
      expect(JSON.stringify(backend.state)).not.toMatch(/private-output|private-install-error/);
      const receipt = readBetaState(directory)!;
      writeFileSync(path.join(directory, "managed-local.json"), JSON.stringify({ ...receipt,
        failure: { ...receipt.failure, coreHookFailures: ["private-output"] } }));
      expect(() => readBetaState(directory)).toThrow("Invalid retained Core hook diagnostics");
    } finally { backend.dispose(); reopened.dispose(); rmSync(root, { recursive: true, force: true }); }
  });
  it("accepts a verified arm64 schema image before authorization", async () => {
    const { backend, dependencies } = fixture({ architecture: async () => "arm64" });
    try {
      expect((await backend.prerequisites({ bridge: true, ports: { https: 8443, bridge: 3000, headlamp: 4466 } })).prerequisitesReady).toBe(true);
      expect(backend.state.checks.find(check => check.id === "architecture")).toMatchObject({ status: "passed", detail: expect.stringContaining("linux/arm64") });
      expect(dependencies.authorizeTerminal).not.toHaveBeenCalled();
    } finally { backend.dispose(); }
  });
  it("reopens only validated inactive failures for inspection, never replays installation or deletes on open", async () => {
    const root = mkdtempSync(path.join(tmpdir(), "kars-reopen-"));
    const directory = betaDirectory(draft().clusterName, root);
    mkdirSync(directory, { mode: 0o700 });
    const receipt = { version: 1 as const, owner: randomUUID(), cluster: draft().clusterName, phase: "failed" as const,
      runtime: "podman", endpoint: "unix:///owned.sock", nodeId: "a".repeat(64) };
    const { backend, calls, dependencies } = fixture({ root });
    try {
      await expect(backend.recoverFailedAttempt("../foreign")).rejects.toThrow("Cannot validate");
      for (const phase of ["ready", "installing", "deleted"] as const) {
        writeBetaState(directory, { ...receipt, phase });
        await expect(backend.recoverFailedAttempt(receipt.cluster)).rejects.toThrow("recorded failed installation");
      }
      writeBetaState(directory, { ...receipt, setupPid: process.pid });
      await expect(backend.recoverFailedAttempt(receipt.cluster)).rejects.toThrow("Close the original");
      writeBetaState(directory, receipt);
      await backend.recoverFailedAttempt(receipt.cluster);
      expect(backend.state).toMatchObject({ phase: "failed", busy: false, recovery: { clusterName: receipt.cluster, cleanupAvailable: true } });
      expect(backend.state.installation).toBeUndefined();
      expect(calls.every(args => args.includes("get"))).toBe(true);
      expect(readBetaState(directory)).toEqual(receipt);
      expect(dependencies.install).not.toHaveBeenCalled();
      expect(dependencies.authorizeTerminal).not.toHaveBeenCalled();
      await expect(backend.install(draft())).rejects.toThrow("consumed");
      await expect(backend.recoverFailedAttempt(receipt.cluster)).rejects.toThrow("already owns");
    } finally { backend.dispose(); rmSync(root, { recursive: true, force: true }); }
  });
  it("blocks unverifiable image platforms before authorization, inference or installation", async () => {
    const { dependencies } = fixture({ architecture: async () => { throw new Error("private-platform-error"); } });
    const backend = new SetupBackend(dependencies);
    expect((await backend.prerequisites({ bridge: true, ports: { https: 8443, bridge: 3000, headlamp: 4466 } })).prerequisitesReady).toBe(false);
    expect(backend.state.checks.find(check => check.id === "architecture")).toMatchObject({ status: "blocked", detail: expect.stringContaining("matching Linux platform") });
    expect(JSON.stringify(backend.state)).not.toContain("private-platform-error");
    expect((await backend.preflight(draft())).phase).toBe("blocked");
    expect(dependencies.authorizeTerminal).not.toHaveBeenCalled();
    expect(dependencies.verifyProvider).not.toHaveBeenCalled();
    expect(dependencies.install).not.toHaveBeenCalled();
    backend.dispose();
  });

  it("requires exact cleanup consent and rejects replaced ownership before any deletion", async () => {
    const root = mkdtempSync(path.join(tmpdir(), "kars-cleanup-binding-"));
    const directory = betaDirectory(draft().clusterName, root), nodeId = "a".repeat(64);
    const { dependencies } = fixture({ root, install: async () => {
      mkdirSync(directory, { mode: 0o700 });
      writeBetaState(directory, { version: 1, owner: randomUUID(), cluster: draft().clusterName, phase: "failed", runtime: "podman", endpoint: "unix:///owned.sock", nodeId });
      throw new Error("retained failure");
    } });
    const execute = dependencies.execute, removed = vi.fn();
    dependencies.execute = async (tool, args, options) => {
      if (args[0] === "rm") removed();
      return execute(tool, args, options);
    };
    const backend = new SetupBackend(dependencies);
    try {
      await backend.preflight(draft());
      await backend.install({ ...draft(), consent: { target: true, hostTrust: true } });
      expect(backend.state.recovery?.cleanupAvailable).toBe(true);
      for (const input of [{ clusterName: "foreign", confirm: true }, { clusterName: draft().clusterName }, { clusterName: draft().clusterName, confirm: true, directory: "/foreign" }]) {
        await expect(backend.cleanup(input)).rejects.toThrow("Explicitly confirm");
      }
      writeBetaState(directory, { ...readBetaState(directory)!, owner: randomUUID() });
      await expect(backend.cleanup({ clusterName: draft().clusterName, confirm: true })).rejects.toThrow("Ownership changed");
      expect(removed).not.toHaveBeenCalled();
    } finally { backend.dispose(); rmSync(root, { recursive: true, force: true }); }
  });

  it("waits for cleanup authorization, deletes by immutable ID and resumes partial trust cleanup without deleting twice", async () => {
    const root = mkdtempSync(path.join(tmpdir(), "kars-cleanup-trust-"));
    const directory = betaDirectory(draft().clusterName, root), nodeId = "a".repeat(64);
    const certificate = path.join(directory, "oidc-ca.pem");
    let authorized = true, finish!: () => void, failTrust = true;
    const { dependencies } = fixture({ root, authorizeTerminal: async () => new Promise<void>(resolve => { finish = () => { authorized = true; resolve(); }; }),
      install: async () => {
        mkdirSync(directory, { mode: 0o700 });
        execFileSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=Kars Kind CA", "-days", "1",
          "-addext", "basicConstraints=critical,CA:TRUE", "-keyout", path.join(directory, "ca.key"), "-out", certificate], { stdio: "ignore" });
        const owner = randomUUID();
        writeBetaState(directory, { version: 1, owner, cluster: draft().clusterName, phase: "failed", runtime: "podman", endpoint: "unix:///owned.sock", nodeId,
          trust: { certificate, fingerprint: new X509Certificate(readFileSync(certificate)).fingerprint256, hostname: `${draft().clusterName}-${owner.slice(0, 8)}.kars.test`, installed: true } });
        throw new Error("retained failure");
      } });
    const execute = dependencies.execute, removed = vi.fn();
    dependencies.execute = async (tool, args, options) => {
      if (tool === "sudo" && args.includes("true") && !authorized) throw new Error("requires authorization");
      if (args[0] === "ps" && args.some(arg => arg.includes(nodeId))) return { stdout: nodeId };
      if (args[0] === "inspect") return { stdout: JSON.stringify([{ Id: nodeId, Config: { Labels: { "io.x-k8s.kind.cluster": draft().clusterName } } }]) };
      if (args[0] === "rm") { removed(tool, args, options); return { stdout: "" }; }
      if (tool === "security" || (tool === "sudo" && args[0] !== "-n")) {
        if (failTrust) throw new Error("private-trust-error");
        return { stdout: "" };
      }
      return execute(tool, args, options);
    };
    const backend = new SetupBackend(dependencies);
    try {
      await backend.preflight(draft());
      await backend.install({ ...draft(), consent: { target: true, hostTrust: true } });
      authorized = false;
      const payload = { clusterName: draft().clusterName, confirm: true };
      const cleaning = backend.cleanup(payload);
      await vi.waitFor(() => expect(backend.state).toMatchObject({ busy: true, authorization: "waiting", cleanup: { status: "running" } }));
      expect(removed).not.toHaveBeenCalled();
      await expect(backend.cleanup(payload)).rejects.toThrow("completed failed attempt");
      finish(); await cleaning;
      expect(backend.state.cleanup?.status).toBe("failed");
      expect(readBetaState(directory)).toMatchObject({ phase: "deleted", trust: { installed: true } });
      expect(removed).toHaveBeenCalledOnce();
      expect(removed).toHaveBeenCalledWith("podman", ["rm", "--force", "--volumes", nodeId], expect.objectContaining({ env: expect.objectContaining({ CONTAINER_HOST: "unix:///owned.sock" }) }));
      expect(JSON.stringify(backend.state)).not.toContain("private-trust-error");
      expect(backend.state.recovery?.cleanupAvailable).toBe(true);
      expect(backend.state.recovery?.nodeRecorded).toBe(false);
      expect(backend.state.recovery?.hostTrustInstalled).toBe(true);
      failTrust = false;
      await backend.cleanup(payload);
      expect(backend.state.cleanup?.status).toBe("passed");
      expect(backend.state.recovery?.cleanupAvailable).toBe(false);
      expect(readBetaState(directory)).toMatchObject({ phase: "deleted", trust: { installed: false } });
      expect(removed).toHaveBeenCalledOnce();
      await expect(backend.install({ ...draft(), consent: { target: true, hostTrust: true } })).rejects.toThrow("consumed");
    } finally { backend.dispose(); rmSync(root, { recursive: true, force: true }); }
  });
  it.each([true, false])("waits for terminal authorization and advances the same preflight only on approval: %s", async approve => {
    let authorized = false, finish!: () => void, deny!: () => void;
    const { dependencies } = fixture({
      authorizeTerminal: vi.fn(() => new Promise<void>((resolve, reject) => {
        finish = () => { authorized = true; resolve(); };
        deny = () => reject(new Error("private-terminal-output"));
      })),
    });
    const execute = dependencies.execute;
    dependencies.execute = async (tool, args, options) => {
      if (tool === "sudo" && args.includes("true") && !authorized) throw new Error("needs authorization");
      return execute(tool, args, options);
    };
    const backend = new SetupBackend(dependencies);
    const operation = backend.preflight(draft());
    await vi.waitFor(() => expect(backend.state).toMatchObject({ busy: true, phase: "preflight", authorization: "waiting", authorizationTarget: "terminal" }));
    expect(dependencies.verifyProvider).not.toHaveBeenCalled();
    expect(dependencies.install).not.toHaveBeenCalled();
    await expect(backend.preflight(draft())).rejects.toThrow("Another setup operation");
    if (approve) finish(); else deny();
    await operation;
    expect(backend.state).toMatchObject({ busy: false, phase: approve ? "review" : "blocked", authorization: approve ? "approved" : "failed" });
    expect(dependencies.verifyProvider).toHaveBeenCalledTimes(approve ? 1 : 0);
    expect(dependencies.authorizeTerminal).toHaveBeenCalledOnce();
    expect(JSON.stringify(backend.state)).not.toContain("private-terminal-output");
    await expect(backend.install(draft())).rejects.toThrow("explicitly consent");
    expect(dependencies.install).not.toHaveBeenCalled();
  });
  it("reuses valid terminal authorization without prompting", async () => {
    const { backend, dependencies } = fixture();
    expect((await backend.preflight(draft())).phase).toBe("review");
    expect(dependencies.authorizeTerminal).not.toHaveBeenCalled();
    expect(backend.state.authorization).toBeUndefined();
  });
  it("cancels terminal authorization with the installer and never advances preflight", async () => {
    const { dependencies } = fixture({ authorizeTerminal: signal => new Promise((_resolve, reject) => {
      signal.addEventListener("abort", () => reject(new Error("cancelled")), { once: true });
    }) });
    const execute = dependencies.execute;
    dependencies.execute = async (tool, args, options) => {
      if (tool === "sudo" && args.includes("true")) throw new Error("needs authorization");
      return execute(tool, args, options);
    };
    const backend = new SetupBackend(dependencies);
    const operation = backend.preflight(draft());
    await vi.waitFor(() => expect(backend.state.authorization).toBe("waiting"));
    backend.dispose();
    await operation;
    expect(backend.state).toMatchObject({ phase: "blocked", busy: false, authorization: "failed" });
    expect(dependencies.verifyProvider).not.toHaveBeenCalled();
    expect(dependencies.install).not.toHaveBeenCalled();
  });
  it("renews expired authorization before a privileged install command, never by replaying it", async () => {
    let authorized = true, finish!: () => void, writes = 0;
    const { dependencies } = fixture({
      authorizeTerminal: vi.fn(() => new Promise<void>(resolve => { finish = () => { authorized = true; resolve(); }; })),
      install: async (_cluster, _tools, _hooks, input) => {
        authorized = false;
        await input!.execute!("sudo", ["owned-hosts-update"]);
      },
    });
    const execute = dependencies.execute;
    dependencies.execute = async (tool, args, options) => {
      if (tool === "sudo" && args.includes("true") && !authorized) throw new Error("expired");
      if (tool === "sudo" && args[0] === "owned-hosts-update") { expect(authorized).toBe(true); writes++; }
      return execute(tool, args, options);
    };
    const backend = new SetupBackend(dependencies);
    await backend.preflight(draft());
    const operation = backend.install({ ...draft(), consent: { target: true, hostTrust: true } });
    await vi.waitFor(() => expect(backend.state).toMatchObject({ authorization: "waiting", authorizationTarget: "terminal", busy: true }));
    expect(writes).toBe(0);
    finish(); await operation;
    expect(writes).toBe(1);
    expect(dependencies.authorizeTerminal).toHaveBeenCalledOnce();
    expect(backend.state.authorization).toBe("approved");
  });
  it("does not run a privileged command when shutdown interrupts the cached authorization check", async () => {
    let checkingInstall = false, finish!: () => void, writes = 0;
    const { dependencies } = fixture({
      install: async (_cluster, _tools, _hooks, input) => {
        checkingInstall = true;
        await input!.execute!("sudo", ["owned-hosts-update"]);
      },
    });
    const execute = dependencies.execute;
    dependencies.execute = async (tool, args, options) => {
      if (tool === "sudo" && args.includes("true") && checkingInstall) await new Promise<void>(resolve => { finish = resolve; });
      if (tool === "sudo" && args[0] === "owned-hosts-update") writes++;
      return execute(tool, args, options);
    };
    const backend = new SetupBackend(dependencies);
    await backend.preflight(draft());
    const operation = backend.install({ ...draft(), consent: { target: true, hostTrust: true } });
    await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
    backend.dispose(); finish(); await operation;
    expect(writes).toBe(0);
    expect(dependencies.authorizeTerminal).not.toHaveBeenCalled();
    expect(backend.state.phase).toBe("failed");
  });
  it("keeps recovery bound to the launching package and safely quotes shell metacharacters", () => {
    const root = mkdtempSync(path.join(tmpdir(), "kars-cleanup-quote-"));
    try {
      const cli = path.join(root, "Kars user's $(printf substituted).mjs");
      writeFileSync(cli, "console.log(JSON.stringify(process.argv.slice(2)))");
      const command = setupCleanupCommand("fresh-cluster", process.execPath, cli);
      expect(JSON.parse(execFileSync("/bin/sh", ["-c", command], { encoding: "utf8" })))
        .toEqual(["dev", "down", "--target", "local-k8s", "--cluster-name", "fresh-cluster"]);
      expect(setupCleanupCommand("fresh-cluster")).not.toMatch(/^kars /);
    } finally { rmSync(root, { recursive: true, force: true }); }
  });
  it("waits for macOS authorization without a deadline and keeps ordinary tool checks bounded", async () => {
    vi.mocked(execa).mockResolvedValue({ stdout: "" } as Awaited<ReturnType<typeof execa>>);
    await executeSetup("/usr/bin/security", ["add-trusted-cert", "owned-ca.pem"]);
    expect(execa).toHaveBeenLastCalledWith("/usr/bin/security", ["add-trusted-cert", "owned-ca.pem"], expect.objectContaining({ timeout: 0, stdio: "pipe" }));
    await executeSetup("/usr/bin/security", ["find-certificate", "-a"]);
    expect(execa).toHaveBeenLastCalledWith("/usr/bin/security", ["find-certificate", "-a"], expect.objectContaining({ timeout: 30_000 }));
  });
  it("reports unsuccessful authorization without leaking command output", async () => {
    vi.mocked(execa).mockRejectedValue(new Error("private-command-output"));
    await expect(executeSetup("security", ["add-trusted-cert", "owned-ca.pem"])).rejects.toThrow("macOS certificate-trust authorization did not complete");
  });
  it.each([true, false])("waits for the actual authorization result and continues only on approval: %s", async approve => {
    let resolve!: (value: { stdout: string }) => void, reject!: (error: Error) => void;
    let continued = false;
    const { backend, dependencies } = fixture({ install: async (_cluster, _tools, _hooks, input) => {
      input!.progress!("dex");
      await input!.execute!("security", ["add-trusted-cert", "owned-ca.pem"]);
      continued = true;
      input!.progress!("core");
    } });
    const execute = dependencies.execute;
    dependencies.execute = async (tool, args, options) => {
      if (tool === "security") return new Promise((yes, no) => { resolve = yes; reject = no; });
      return execute(tool, args, options);
    };
    const isolated = new SetupBackend(dependencies);
    await isolated.preflight(draft());
    const installation = isolated.install({ ...draft(), consent: { target: true, hostTrust: true } });
    await vi.waitFor(() => expect(isolated.state.authorization).toBe("waiting"));
    expect(isolated.state.busy).toBe(true);
    expect(continued).toBe(false);
    if (approve) resolve({ stdout: "" }); else reject(new Error("private-denial"));
    await installation;
    expect(continued).toBe(approve);
    expect(isolated.state.authorization).toBe(approve ? "approved" : "failed");
    expect(JSON.stringify(isolated.state)).not.toContain("private-denial");
    backend.dispose();
  });
  it("cancels a pending system authorization when the launching installer closes", async () => {
    const { dependencies } = fixture({ install: async (_cluster, _tools, _hooks, input) => {
      await input!.execute!("security", ["add-trusted-cert", "owned-ca.pem"]);
    } });
    const execute = dependencies.execute;
    dependencies.execute = async (tool, args, options) => {
      if (tool === "security") return new Promise((_resolve, reject) => {
        options!.cancelSignal!.addEventListener("abort", () => reject(new Error("cancelled")), { once: true });
      });
      return execute(tool, args, options);
    };
    const backend = new SetupBackend(dependencies);
    await backend.preflight(draft());
    const installation = backend.install({ ...draft(), consent: { target: true, hostTrust: true } });
    await vi.waitFor(() => expect(backend.state.authorization).toBe("waiting"));
    backend.dispose(); await installation;
    expect(backend.state.authorization).toBe("failed");
    expect(backend.state.busy).toBe(false);
  });
  it.each([443, 65536, 1024.5, "8443", null])("rejects invalid local HTTPS port %s before checking providers", https => {
    expect(() => parseSetupDraft({ ...draft(), ports: { https, bridge: 3000, headlamp: 4466 } })).toThrow(/ports/);
  });
  it("rejects duplicate active ports but permits an unused Bridge port in Core-only setup", () => {
    const input = { ...draft(), ports: { https: 8443, bridge: 4466, headlamp: 4466 } };
    expect(() => parseSetupDraft(input)).toThrow(/different/);
    expect(parseSetupDraft({ ...input, bridge: false }).ports.headlamp).toBe(4466);
  });
  it("binds custom ports to approval and hands off only to the selected UI port", async () => {
    const input = { ...draft(), ports: { https: 9443, bridge: 3100, headlamp: 4566 } };
    const { backend, dependencies } = fixture({ install: async (_cluster, _tools, hooks, selected) => {
      expect(selected!.ports).toEqual(input.ports);
      await hooks.openBrowser!("http://localhost:3100");
      expect(backend.state.handoffUrl).toBe("http://localhost:3100");
    } });
    await backend.preflight(input);
    expect(vi.mocked(dependencies.port).mock.calls.map(call => call[0])).toEqual([9443, 3100, 4566]);
    await expect(backend.install({ ...input, ports: { ...input.ports, https: 9444 }, consent: { target: true, hostTrust: true } })).rejects.toThrow("exact inputs");
    await backend.install({ ...input, consent: { target: true, hostTrust: true } });
    expect(backend.state.installation).toMatchObject({ clusterName: input.clusterName, ports: input.ports });
  });
  it("reports the unavailable port without exposing command output or testing inference", async () => {
    const { backend, dependencies } = fixture({ port: async () => { throw new Error("private-runtime-output"); } });
    await backend.preflight(draft());
    expect(JSON.stringify(backend.state)).toContain("Loopback port 8443 is unavailable");
    expect(JSON.stringify(backend.state)).not.toContain("private-runtime-output");
    expect(dependencies.verifyProvider).not.toHaveBeenCalled();
  });
  it("publishes real stage transitions and ownership-based recovery without replaying a failure", async () => {
    const root = mkdtempSync(path.join(tmpdir(), "kars-setup-recovery-"));
    let release!: () => void;
    const { backend } = fixture({ root, install: async (cluster, _tools, _hooks, input) => {
      const directory = betaDirectory(cluster, root); mkdirSync(directory, { mode: 0o700 });
      writeBetaState(directory, { version: 1, cluster, owner: randomUUID(), runtime: "podman", endpoint: "unix:///fixture.sock", phase: "failed", nodeId: "a".repeat(64) });
      input!.progress!("identity"); input!.progress!("cluster");
      await new Promise<void>(resolve => { release = resolve; });
      throw new Error("private-failure-output");
    } });
    try {
      await backend.preflight(draft());
      const operation = backend.install({ ...draft(), consent: { target: true, hostTrust: true } });
      await vi.waitFor(() => expect(release).toBeDefined());
      expect(backend.state.steps[0]).toMatchObject({ status: "passed", finishedAt: expect.any(Number) });
      expect(backend.state.steps[1]).toMatchObject({ id: "cluster", status: "running", startedAt: expect.any(Number) });
      release(); await operation;
      expect(backend.state.steps[1].status).toBe("failed");
      expect(backend.state.recovery).toMatchObject({ receipt: "available", nodeRecorded: true, hostTrustInstalled: false,
        cleanupCommand: setupCleanupCommand("concierge-unit-fixture") });
      expect(backend.state.installation?.finishedAt).toEqual(expect.any(Number));
      expect(JSON.stringify(backend.state)).not.toMatch(/private-failure-output|fixture-key|Unit-fixture-password/);
      await expect(backend.install(draft())).rejects.toThrow();
    } finally { rmSync(root, { recursive: true, force: true }); }
  });
  it("does not invent retained resources when failure precedes receipt creation", async () => {
    const { backend } = fixture();
    await backend.preflight(draft()); await backend.install({ ...draft(), consent: { target: true, hostTrust: true } });
    expect(backend.state.recovery).toMatchObject({ receipt: "absent" });
    expect(backend.state.recovery?.cleanupCommand).toBeUndefined();
  });
  it("withholds cleanup when the ownership receipt cannot be read", async () => {
    const root = mkdtempSync(path.join(tmpdir(), "kars-setup-receipt-"));
    const { backend } = fixture({ root, install: async cluster => {
      const directory = betaDirectory(cluster, root);
      mkdirSync(path.join(directory, "managed-local.json"), { recursive: true, mode: 0o700 });
      throw new Error("fixture failure");
    } });
    try {
      await backend.preflight(draft()); await backend.install({ ...draft(), consent: { target: true, hostTrust: true } });
      expect(backend.state.recovery).toMatchObject({ receipt: "unreadable", workingDirectory: root });
      expect(backend.state.recovery?.cleanupCommand).toBeUndefined();
      expect(backend.state.recovery?.nodeRecorded).toBeUndefined();
    } finally { rmSync(root, { recursive: true, force: true }); }
  });
  it("accepts strong passwords through 72 UTF-8 bytes, not 72 multibyte characters", () => {
    const value = draft();
    expect(() => parseSetupDraft({ ...value, admin: { ...value.admin, password: "A1!" + "a".repeat(69) } })).not.toThrow();
    expect(() => parseSetupDraft({ ...value, admin: { ...value.admin, password: "A1!" + "a".repeat(70) } })).toThrow();
    expect(() => parseSetupDraft({ ...value, admin: { ...value.admin, password: "Aa1!" + "é".repeat(34) } })).not.toThrow();
    expect(() => parseSetupDraft({ ...value, admin: { ...value.admin, password: "Aa1!" + "é".repeat(35) } })).toThrow();
  });
  it("checks prerequisites without credentials, sudo authorization or inference, and cannot authorize installation", async () => {
    const { backend, calls, dependencies } = fixture();
    const state = await backend.prerequisites();
    expect(state).toMatchObject({ phase: "draft", busy: false, prerequisitesReady: true });
    expect(state.checks.map(check => check.id)).toEqual(["port-https", "port-bridge", "port-headlamp", "tool-kind", "tool-podman", "tool-kubectl", "tool-helm", "tool-openssl", "tool-sudo", "runtime", "architecture", "assets", "host"]);
    expect(state.checks.every(check => check.status === "passed")).toBe(true);
    expect(calls.some(args => args.includes("true") || args.includes("create") || args.includes("install"))).toBe(false);
    expect(dependencies.verifyProvider).not.toHaveBeenCalled();
    await expect(backend.install({ ...draft(), consent: { target: true, hostTrust: true } })).rejects.toThrow("Run preflight");
  });
  it("detects a genuinely occupied loopback port before credentials and passes after it is released", async () => {
    const listeners = Array.from({ length: 3 }, () => createServer());
    const close = (server: typeof listeners[number]) => new Promise<void>((resolve, reject) => {
      if (!server.listening) { resolve(); return; }
      server.close(error => error ? reject(error) : resolve());
    });
    try {
      const numbers = await Promise.all(listeners.map(server => new Promise<number>((resolve, reject) => {
        server.once("error", reject);
        server.listen(0, "127.0.0.1", () => {
          const address = server.address();
          if (!address || typeof address === "string") { reject(new Error("Missing fixture port")); return; }
          resolve(address.port);
        });
      })));
      const ports = { https: numbers[0], bridge: numbers[1], headlamp: numbers[2] };
      await close(listeners[0]); await close(listeners[2]);
      const { backend, dependencies } = fixture({ port: assertPortAvailable });
      const state = await backend.prerequisites({ bridge: true, ports });
      expect(state).toMatchObject({ phase: "blocked", prerequisitesReady: false, prerequisiteSelection: { bridge: true, ports } });
      expect(state.checks.filter(check => check.id.startsWith("port-")).map(check => check.status)).toEqual(["passed", "blocked", "passed"]);
      expect(state.checks[1].detail).toContain(`Loopback port ${ports.bridge} is busy or cannot be bound`);
      expect(listeners[1].listening).toBe(true);
      expect(dependencies.verifyProvider).not.toHaveBeenCalled();
      expect(dependencies.install).not.toHaveBeenCalled();
      await close(listeners[1]);
      expect((await backend.prerequisites({ bridge: true, ports })).prerequisitesReady).toBe(true);
    } finally { await Promise.all(listeners.map(close)); }
  });
  it("checks custom active ports only and safely reports each unavailable port", async () => {
    const port = vi.fn(async (number: number) => { if (number === 3100) throw new Error("private-process-output"); });
    const { backend } = fixture({ port });
    const ports = { https: 9443, bridge: 3100, headlamp: 4566 };
    expect((await backend.prerequisites({ bridge: true, ports })).prerequisitesReady).toBe(false);
    expect(port.mock.calls.map(([number]) => number)).toEqual([9443, 3100, 4566]);
    expect(JSON.stringify(backend.state)).not.toContain("private-process-output");
    port.mockClear();
    expect((await backend.prerequisites({ bridge: false, ports })).prerequisitesReady).toBe(true);
    expect(port.mock.calls.map(([number]) => number)).toEqual([9443, 4566]);
    expect(backend.state.checks.some(check => check.id === "port-bridge")).toBe(false);
  });
  it.each([
    { bridge: "false" },
    { ports: { https: 443, bridge: 3000, headlamp: 4466 } },
    { ports: { https: 8443, bridge: 8443, headlamp: 4466 } },
  ])("rejects invalid prerequisite port selections before probing: %j", async input => {
    const { backend, dependencies, calls } = fixture();
    await expect(backend.prerequisites(input)).rejects.toThrow(SetupError);
    expect(dependencies.port).not.toHaveBeenCalled();
    expect(calls).toEqual([]);
    expect(backend.state.prerequisitesReady).toBe(false);
  });
  it("publishes running checks, excludes concurrent operations, reports missing tools, and allows retry", async () => {
    let release!: () => void;
    const f = fixture(), original = f.dependencies.execute;
    let fail = true;
    const backend = new SetupBackend({ ...f.dependencies, execute: async (tool, args, options) => {
      if (tool === "which" && args[0] === "kubectl" && fail) {
        await new Promise<void>(resolve => { release = resolve; }); throw new Error("private-path");
      }
      return original(tool, args, options);
    } });
    const pending = backend.prerequisites();
    await vi.waitFor(() => expect(release).toBeDefined());
    expect(backend.state).toMatchObject({ busy: true, phase: "prerequisites", prerequisitesReady: false });
    expect(backend.state.checks.at(-1)).toMatchObject({ id: "tool-kubectl", status: "running" });
    await expect(backend.prerequisites()).rejects.toThrow("Cannot check");
    await expect(backend.preflight(draft())).rejects.toThrow("Another setup operation");
    release(); await pending;
    expect(backend.state.prerequisitesReady).toBe(false);
    expect(backend.state.checks.find(check => check.id === "tool-kubectl")).toMatchObject({ status: "blocked", detail: expect.stringContaining("was not found") });
    expect(backend.state.checks.find(check => check.id === "runtime")?.status).toBe("passed");
    expect(JSON.stringify(backend.state)).not.toContain("private-path");
    fail = false;
    expect((await backend.prerequisites()).prerequisitesReady).toBe(true);
    expect(f.dependencies.verifyProvider).not.toHaveBeenCalled();
  });
  it("distinguishes an installed Podman CLI from an unavailable service", async () => {
    const f = fixture(), original = f.dependencies.execute;
    const backend = new SetupBackend({ ...f.dependencies, execute: async (tool, args, options) => {
      if (args[0] === "info") throw new Error("private-connection-details");
      return original(tool, args, options);
    } });
    const state = await backend.prerequisites();
    expect(state.checks.find(check => check.id === "tool-podman")?.status).toBe("passed");
    expect(state.checks.find(check => check.id === "runtime")).toMatchObject({ status: "blocked", detail: expect.stringContaining("podman machine list") });
    expect(state.prerequisitesReady).toBe(false);
    expect(JSON.stringify(state)).not.toContain("private-connection-details");
  });
  it("revokes a previous review when prerequisites are checked again", async () => {
    const { backend } = fixture(); await backend.preflight(draft());
    await backend.prerequisites();
    await expect(backend.install({ ...draft(), consent: { target: true, hostTrust: true } })).rejects.toThrow("Run preflight");
  });
  it.each(["simplelowercasepassword", "NoNumbersHere!", "NoSymbolsHere123", "ALLUPPERCASE123!", "A1!" + "é".repeat(35)])("enforces password requirements server-side: %s", password => {
    expect(() => parseSetupDraft({ ...draft(), admin: { ...draft().admin, password } })).toThrow(SetupError);
  });
  it.each(["openai", "anthropic", "openai-compatible"] as const)("passes %s through the shared installer without borrowing Azure credentials", async kind => {
    let captured: unknown;
    const { backend } = fixture({ install: async (_cluster, _tools, _hooks, input) => {
      captured = structuredClone(input!.providerConnection);
      expect(input!.provider).toBe(kind);
      expect(input!.inference!.load()).toBeNull();
    } });
    const provider = { kind, endpoint: kind === "openai-compatible" ? "https://models.example.com/v1" : `https://api.${kind}.com/v1`, model: "chosen-model", apiKey: "private-fixture" };
    const input = { ...draft(), provider };
    await backend.preflight(input);
    await backend.install({ ...input, consent: { target: true, hostTrust: true } });
    expect(captured).toEqual(provider);
    expect(JSON.stringify(backend.state)).not.toContain(provider.apiKey);
  });
  it("injects only installer-owned Copilot credentials and invalidates approval on logout", async () => {
    const login = new SetupCopilotLogin(async () => "private-oauth", models);
    const { backend, dependencies } = fixture({}, login);
    const input = { ...draft(), provider: { kind: "github-copilot", endpoint: "https://api.githubcopilot.com", model: "gpt-5" } };
    await expect(backend.preflight(input)).rejects.toThrow("Sign in");
    backend.startCopilot(); await vi.waitFor(() => expect(backend.state.copilot?.phase).toBe("connected"));
    await expect(backend.preflight({ ...input, provider: { ...input.provider, apiKey: "pasted" } })).rejects.toThrow("rather than pasting");
    await backend.preflight(input);
    expect(dependencies.verifyProvider).toHaveBeenCalledWith(expect.objectContaining({ kind: "github-copilot" }));
    expect(JSON.stringify(backend.state)).not.toContain("private-oauth");
    backend.cancelCopilot();
    await expect(backend.install({ ...input, consent: { target: true, hostTrust: true } })).rejects.toThrow("Sign in");
    expect(dependencies.install).not.toHaveBeenCalled();
  });
  it("passes the private Copilot credential only to the shared installer", async () => {
    const login = new SetupCopilotLogin(async () => "private-oauth", models);
    let captured: unknown;
    const { backend } = fixture({ install: async (_cluster, _tools, _hooks, input) => {
      expect(input!.provider).toBe("github-copilot");
      expect(input!.inference!.load()).toBeNull();
      captured = structuredClone(input!.providerConnection);
    } }, login);
    backend.startCopilot();
    await vi.waitFor(() => expect(backend.state.copilot?.phase).toBe("connected"));
    const input = { ...draft(), provider: { kind: "github-copilot", endpoint: "https://api.githubcopilot.com", model: "gpt-5" } };
    await backend.preflight(input);
    await backend.install({ ...input, consent: { target: true, hostTrust: true } });
    expect(captured).toEqual({ ...input.provider, apiKey: "private-oauth" });
    expect(JSON.stringify(backend.state)).not.toContain("private-oauth");
    backend.dispose();
    expect(login.state.phase).toBe("idle");
  });
  it("requires exact local identity and target inputs", () => {
    expect(parseSetupDraft(draft()).clusterName).toBe("concierge-unit-fixture");
    for (const bad of [null, [], { ...draft(), clusterName: "../escape" }, { ...draft(), admin: null }, { ...draft(), admin: { email: "bad", password: "short" } }]) {
      expect(() => parseSetupDraft(bad)).toThrow(SetupError);
    }
  });
  it.each([
    "http://fixture.openai.azure.com", "https://localhost", "https://fixture.openai.azure.com.evil.example",
    "https://fixture.openai.azure.com:8443", "https://user:password@fixture.openai.azure.com",
    "https://fixture.openai.azure.com/?key=secret", "https://fixture.openai.azure.com/api/projects/project",
  ])("rejects unsafe or unsupported provider endpoint %s", endpoint => {
    expect(() => validateProvider({ ...draft().provider, endpoint })).toThrow(SetupError);
  });
  it("performs read-only preflight and does not leak or clear caller credentials", async () => {
    const { backend, calls, dependencies } = fixture(); const input = draft();
    const state = await backend.preflight(input);
    expect(state.phase).toBe("review"); expect(state.busy).toBe(false);
    expect(state.checks.every(check => check.status === "passed")).toBe(true);
    expect(dependencies.verifyProvider).toHaveBeenCalledOnce();
    expect(JSON.stringify(state)).not.toContain("fixture-key");
    expect(input.provider.apiKey).toBe("fixture-key");
    expect(calls.flat()).not.toContain("create"); expect(calls.flat()).not.toContain("install");
    expect(dependencies.install).not.toHaveBeenCalled();
  });
  it("does not make billable inference requests if local prerequisites fail", async () => {
    const { backend, dependencies } = fixture({ assets: () => { throw new Error("asset failure with secret"); } });
    const state = await backend.preflight(draft());
    expect(state.phase).toBe("blocked"); expect(JSON.stringify(state)).not.toContain("asset failure with secret");
    expect(dependencies.verifyProvider).not.toHaveBeenCalled();
    await expect(backend.install({ ...draft(), consent: { target: true, hostTrust: true } })).rejects.toThrow("Run preflight");
  });
  it("blocks before install when the provider cannot return a model response", async () => {
    const { backend } = fixture({ verifyProvider: async () => { throw new SetupError("HTTP 401; check your key"); } });
    const state = await backend.preflight(draft());
    expect(state.phase).toBe("blocked"); expect(state.checks.at(-1)?.detail).toContain("401");
  });
  it.each(["target", "hostTrust"])("requires explicit %s consent", async field => {
    const { backend, dependencies } = fixture(); await backend.preflight(draft());
    await expect(backend.install({ ...draft(), consent: { target: true, hostTrust: true, [field]: false } })).rejects.toThrow("explicitly consent");
    expect(dependencies.install).not.toHaveBeenCalled();
  });
  it("invalidates approval when any provider or target input changes", async () => {
    const { backend, dependencies } = fixture(); await backend.preflight(draft());
    await expect(backend.install({ ...draft(), clusterName: "different", consent: { target: true, hostTrust: true } })).rejects.toThrow("exact inputs");
    expect(dependencies.install).not.toHaveBeenCalled();
  });
  it("consumes failed attempts and preserves safe failure state instead of replaying", async () => {
    const { backend, dependencies } = fixture(); await backend.preflight(draft());
    const approved = { ...draft(), consent: { target: true, hostTrust: true } };
    await backend.install(approved);
    expect(backend.state.phase).toBe("failed"); expect(backend.state.steps.some(step => step.status === "failed")).toBe(true);
    expect(dependencies.install).toHaveBeenCalledOnce();
    await expect(backend.install(approved)).rejects.toThrow("consumed");
    await expect(backend.preflight(draft())).rejects.toThrow("consumed");
  });
  it("checks the selected AKS context without changing it or modifying existing Core", async () => {
    const { backend, calls, dependencies } = fixture(); await backend.initialize();
    const state = await backend.preflight({ ...draft(), target: "existing", context: "fixture-aks", admin: undefined, provider: undefined });
    expect(state.phase).toBe("blocked"); expect(state.checks.find(check => check.id === "core")?.detail).toContain("Existing Core detected");
    expect(state.providerConfigured).toBe(false);
    expect(state.core).toMatchObject({ context: "fixture-aks", provider: "openai", model: "existing-model" });
    expect(JSON.stringify(state)).not.toContain("private-existing-key");
    expect(calls.filter(args => args[0] === "helm").every(args => args.slice(1, 3).join(" ") === "--kube-context fixture-aks")).toBe(true);
    expect(state.checks.at(-1)?.detail).toContain("not supported yet");
    expect(calls.some(args => args.includes("--context") && args.includes("fixture-aks"))).toBe(true);
    expect(calls.flat()).not.toContain("use-context"); expect(dependencies.install).not.toHaveBeenCalled();
  });
  it.each(["missing-context", "local", "invalid-draft"])("clears discovered defaults before a %s retry", async retry => {
    const { backend } = fixture();
    await backend.preflight({ ...draft(), target: "existing", context: "fixture-aks", admin: undefined, provider: undefined });
    expect(backend.state.core?.provider).toBe("openai");
    if (retry === "invalid-draft") await expect(backend.preflight(null)).rejects.toThrow("Invalid setup draft");
    else await backend.preflight(retry === "local" ? draft() : { ...draft(), target: "existing", context: "missing-context" });
    expect(backend.state.core).toBeUndefined();
    expect(backend.state.providerConfigured).toBe(false);
    expect(JSON.stringify(backend.state)).not.toContain("existing-model");
  });
  it("redacts Helm discovery errors and leaves configuration unverified", async () => {
    const f = fixture(); const original = f.dependencies.execute;
    f.dependencies.execute = async (tool, args, options) => {
      if (tool === "helm") throw Error("private-existing-token");
      return original(tool, args, options);
    };
    const backend = new SetupBackend(f.dependencies);
    const state = await backend.preflight({ ...draft(), target: "existing", context: "fixture-aks" });
    expect(state.checks.find(check => check.id === "core")?.status).toBe("blocked");
    expect(state.core).toBeUndefined(); expect(state.providerConfigured).toBe(false);
    expect(JSON.stringify(state)).not.toContain("private-existing-token");
  });
  it("requires a secure Headlamp account even without Bridge", () => {
    expect(() => parseSetupDraft({ ...draft(), bridge: false, admin: undefined })).toThrow(SetupError);
  });
  it.each(["foundry", "azure-openai"] as const)("preflights Core-only %s with only Headlamp ports and rejects changed components", async kind => {
    const { backend, dependencies } = fixture(); const input = { ...draft(), bridge: false, provider: { ...draft().provider, kind,
      endpoint: kind === "foundry" ? "https://fixture.services.ai.azure.com" : draft().provider.endpoint } };
    const state = await backend.preflight(input);
    expect(state.phase).toBe("review"); expect(state.message).toContain("no Bridge release");
    expect(dependencies.port).toHaveBeenCalledTimes(2);
    expect(dependencies.port).toHaveBeenCalledWith(8443); expect(dependencies.port).toHaveBeenCalledWith(4466);
    expect(dependencies.port).not.toHaveBeenCalledWith(3000);
    await expect(backend.install({ ...input, bridge: true, consent: { target: true, hostTrust: true } })).rejects.toThrow("exact inputs");
    expect(dependencies.install).not.toHaveBeenCalled();
  });
  it("hands Core-only off to Headlamp, never Bridge", async () => {
    let release!: () => void;
    const { backend } = fixture({ install: async (_cluster, _tools, hooks, input) => {
      expect(input!.includeBridge).toBe(false);
      expect(input!.account.email).toBe(draft().admin.email);
      expect(input!.provider).toBe("azure-openai");
      await hooks.openBrowser!("http://localhost:4466");
      await new Promise<void>(resolve => { release = resolve; });
    } });
    const input = { ...draft(), bridge: false };
    await backend.preflight(input); const operation = backend.install({ ...input, consent: { target: true, hostTrust: true } });
    await Promise.resolve(); expect(backend.state.phase).toBe("ready");
    expect(backend.state.handoffUrl).toBe("http://localhost:4466");
    release(); await operation;
  });
  it.each(["openai", "anthropic", "github-copilot", "openai-compatible"] as const)("reviews Core-only %s without silently adding Bridge", async kind => {
    const { backend, dependencies } = fixture({}, new SetupCopilotLogin(async () => "private-oauth", models));
    const input = { ...draft(), bridge: false, provider: { kind, endpoint: kind === "anthropic" ? "https://api.anthropic.com/v1"
      : kind === "github-copilot" ? "https://api.githubcopilot.com" : "https://api.openai.com/v1",
      model: "test-model", ...(kind === "github-copilot" ? {} : { apiKey: "private-key" }) } };
    if (kind === "github-copilot") { backend.startCopilot(); await vi.waitFor(() => expect(backend.state.copilot?.phase).toBe("connected")); }
    const state = await backend.preflight(input);
    expect(state.phase).toBe("review"); expect(state.message).toContain("no Bridge release");
    expect(dependencies.verifyProvider).toHaveBeenCalledOnce();
    await backend.install({ ...input, consent: { target: true, hostTrust: true } });
    expect(dependencies.install).toHaveBeenCalledWith(expect.any(String), expect.any(Object), expect.any(Object),
      expect.objectContaining({ includeBridge: false, provider: kind }));
  });
  it("admits one asynchronous install, exposes qualified handoff, and retains consumed state", async () => {
    let release!: () => void;
    const { backend } = fixture({ install: async (_cluster, _tools, hooks, input) => {
      input!.progress!("identity"); input!.progress!("cluster"); input!.progress!("access");
      await hooks.openBrowser!("http://localhost:3000");
      await new Promise<void>(resolve => { release = resolve; });
    } });
    await backend.preflight(draft()); const operation = backend.install({ ...draft(), consent: { target: true, hostTrust: true } });
    await Promise.resolve();
    expect(backend.state.phase).toBe("ready"); expect(backend.state.message).toContain("not yet been acceptance-tested");
    expect(backend.state.handoffUrl).toBe("http://localhost:3000");
    release(); await operation; expect(backend.state.phase).toBe("closed"); expect(backend.state.handoffUrl).toBeUndefined();
  });
});

describe("provider connection verification", () => {
  it("requires an actual reply and disables redirects with an abort deadline", async () => {
    const request = vi.fn(async () => new Response(JSON.stringify({ choices: [{ message: { content: "OK" } }] })));
    await verifySetupProvider(draft().provider, request as typeof fetch);
    expect(request).toHaveBeenCalledWith(expect.stringContaining("/openai/deployments/test-model/chat/completions"), expect.objectContaining({ redirect: "error", signal: expect.any(AbortSignal) }));
  });
  it.each([401, 429, 500])("redacts provider HTTP %s bodies", async status => {
    await expect(verifySetupProvider(draft().provider, vi.fn(async () => new Response("sensitive-provider-body", { status })) as typeof fetch)).rejects.toThrow(`HTTP ${status}`);
  });
  it("rejects success responses without a model reply", async () => {
    await expect(verifySetupProvider(draft().provider, vi.fn(async () => new Response("{}")) as typeof fetch)).rejects.toThrow("no model reply");
  });
});
