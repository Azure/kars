// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import type { KarsConfig } from "../../config.js";
import { assertLocalBetaAssets, validateLocalBetaAccount, localBetaPorts, localBetaArchitecture } from "../dev/kind-beta-local.js";
import { assertManagedHostSupport, changeManagedHostTrust } from "../dev/kind-beta-host.js";
import { assertFreshKindBetaCluster, assertPortAvailable, runManagedKindBeta, type BetaExecute, type BetaTools } from "../dev/kind-beta.js";
import { beginBetaState, betaDirectory, pinnedBetaTools, readBetaState, writeBetaState, teardownManagedBeta, type KindBetaState } from "../dev/kind-beta-state.js";
import { ensureCluster, installLocalCoreChart } from "../dev/local-k8s.js";
import { SetupError, type SetupDraft, type SetupState } from "./types.js";
import { validateProvider, verifySetupProvider } from "./providers.js";
import { SetupCopilotLogin } from "./copilot.js";
import { inspectSetupCore } from "./core.js";
import { KindInventoryError } from "../dev/kind-inventory.js";
import { setupPasswordIssue } from "./password.js";
import { observeSetupCluster } from "./progress.js";
import { coreHookFailureMessage, isCoreHookFailureCode } from "../../lib/core-hook-diagnostics.js";
export { validateProvider, verifySetupProvider } from "./providers.js";

// eslint-disable-next-line no-control-regex -- Reject ASCII controls in externally supplied configuration.
const safeText = (value: unknown, max = 512): value is string => typeof value === "string" && value.length <= max && !/[\x00-\x1f\x7f]/.test(value);

export function parseSetupDraft(value: unknown): SetupDraft {
  const draft = value as SetupDraft;
  if (!draft || typeof draft !== "object" || Array.isArray(draft) || !["kind", "existing"].includes(draft.target)
    || typeof draft.bridge !== "boolean" || !draft.consent || typeof draft.consent.target !== "boolean" || typeof draft.consent.hostTrust !== "boolean"
    || (draft.admin !== undefined && (!draft.admin || !safeText(draft.admin.email, 254) || !safeText(draft.admin.password, 256)))) throw new SetupError("Invalid setup draft");
  const clusterName = draft.clusterName ?? "", context = draft.context ?? "";
  if (!safeText(clusterName, 40) || !safeText(context, 253) || (draft.target === "kind" && !/^[a-z0-9][a-z0-9-]{0,39}$/.test(clusterName))
    || (draft.target === "existing" && (!context || context.startsWith("-")))) throw new SetupError("Choose an explicit valid target");
  if (draft.target === "kind") {
    try { validateLocalBetaAccount(draft.admin); } catch { throw new SetupError("Use a valid email and a password of 12+ characters, at most 72 UTF-8 bytes"); }
    const passwordIssue = setupPasswordIssue(draft.admin.password);
    if (passwordIssue) throw new SetupError(passwordIssue);
    validateProvider(draft.provider);
  }
  let ports: SetupDraft["ports"];
  try { ports = localBetaPorts(draft.ports, draft.bridge); } catch (error) { throw new SetupError(error instanceof Error ? error.message : "Invalid local ports"); }
  return { target: draft.target, clusterName, context, bridge: draft.bridge, ports,
    provider: draft.provider ? { kind: draft.provider.kind, endpoint: draft.provider.endpoint, model: draft.provider.model, apiKey: draft.provider.apiKey } : { kind: "azure-openai", endpoint: "", model: "" },
    admin: { email: draft.admin?.email ?? "", password: draft.admin?.password ?? "" }, consent: { ...draft.consent } };
}
const fingerprint = (draft: SetupDraft) => createHash("sha256").update(JSON.stringify({ ...draft, consent: undefined })).digest("hex");

// No command output (including stderr/arguments) crosses the browser boundary.
const isTrustAuthorization = (tool: string, args: string[]) => path.basename(tool) === "security" && ["add-trusted-cert", "remove-trusted-cert"].includes(args[0]);
export const executeSetup: BetaExecute = async (tool, args, options) => {
  const trustPrompt = isTrustAuthorization(tool, args);
  try {
    const versionOnly = args.length === 1 && args[0] === "--version";
    const result = await execa(tool, path.basename(tool) === "sudo" && !versionOnly ? ["-n", ...args] : args,
      { ...options, stdio: "pipe", timeout: trustPrompt ? 0 : args.some(arg => ["install", "upgrade", "create", "rollout", "wait"].includes(arg)) ? 30 * 60_000 : 30_000, maxBuffer: 4 * 1024 * 1024 });
    return { stdout: result.stdout };
  } catch {
    if (trustPrompt) {
      throw new SetupError("macOS certificate-trust authorization did not complete. Check the system authorization dialog and retained trust state; no automatic retry or rollback ran.");
    }
    throw new SetupError(`${path.basename(tool)} operation failed; inspect the owned installation locally. Command output and credentials are not displayed.`);
  }
};

export async function authorizeSetupTerminal(signal: AbortSignal,
  terminal = Boolean(process.stdin.isTTY && process.stderr.isTTY)): Promise<void> {
  if (!terminal) throw new SetupError("Open the installer in an interactive terminal to authorize host access. This process has no terminal for the OS password prompt. Never enter an OS password in the browser.");
  process.stderr.write("\nKars is waiting for host-access authorization. Enter your OS password here, never in the browser. Setup continues automatically after approval.\n");
  try {
    await execa("sudo", ["-v"], { stdio: "inherit", timeout: 0, cancelSignal: signal });
  } catch {
    throw new SetupError("Terminal authorization was cancelled, denied, or failed. No privileged operation was retried. Check the launching terminal; never enter an OS password in the browser.");
  }
}

export function setupCleanupCommand(cluster: string, node = process.execPath,
  cli = fileURLToPath(new URL("../../index.js", import.meta.url))): string {
  const quote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;
  return `${quote(node)} ${quote(cli)} dev down --target local-k8s --cluster-name ${quote(cluster)}`;
}

export interface SetupDependencies {
  execute: BetaExecute;
  authorizeTerminal: (signal: AbortSignal) => Promise<void>;
  assets: () => void;
  architecture: typeof localBetaArchitecture;
  verifyProvider: typeof verifySetupProvider;
  hostSupport: () => void;
  port: (port: number) => Promise<void>;
  install: typeof runManagedKindBeta;
  root: string;
}
const defaults: SetupDependencies = { execute: executeSetup, authorizeTerminal: authorizeSetupTerminal, assets: assertLocalBetaAssets, architecture: localBetaArchitecture, verifyProvider: verifySetupProvider,
  hostSupport: assertManagedHostSupport, port: assertPortAvailable, install: runManagedKindBeta, root: process.cwd() };

export class SetupBackend {
  readonly state: SetupState = { phase: "draft", contexts: [], checks: [], steps: [], busy: false, providerConfigured: false };
  private readonly deps: SetupDependencies;
  private approved?: { hash: string; tools: BetaTools; endpoint: string; sshKey?: string };
  private consumed = false;
  private inventory: any;
  private readonly copilot: SetupCopilotLogin;
  private readonly authorizationAbort = new AbortController();
  private observation?: ReturnType<typeof observeSetupCluster>;
  private cleanupTarget?: { directory: string; receipt: KindBetaState; tools: BetaTools };
  constructor(dependencies: Partial<SetupDependencies> = {}, copilot = new SetupCopilotLogin()) {
    this.deps = { ...defaults, ...dependencies }; this.copilot = copilot; this.state.copilot = copilot.state;
  }
  startCopilot(): SetupState {
    if (this.state.busy || this.consumed) throw new SetupError("Cannot change provider sign-in during or after an installation");
    this.approved = undefined; this.copilot.start(); return this.state;
  }
  cancelCopilot(): SetupState {
    if (this.state.busy) throw new SetupError("Cannot change provider sign-in during a setup operation");
    this.approved = undefined; this.copilot.cancel(); return this.state;
  }
  refreshCopilotModels(): SetupState {
    if (this.state.busy || this.consumed) throw new SetupError("Cannot refresh models during or after an installation");
    this.approved = undefined; this.copilot.refreshModels(); return this.state;
  }
  dispose(): void { this.observation?.stop(); this.copilot.cancel(); this.authorizationAbort.abort(); }

  async recoverFailedAttempt(cluster: string): Promise<void> {
    if (this.consumed || this.state.busy) throw new SetupError("This installer already owns an attempt");
    let receipt: KindBetaState | undefined;
    try { receipt = readBetaState(betaDirectory(cluster, this.deps.root)); }
    catch { throw new SetupError("Cannot validate this working directory's private ownership receipt. No resources changed."); }
    if (!receipt?.nodeId || !["failed", "deleted"].includes(receipt.phase)
      || (receipt.phase === "deleted" && !receipt.trust?.installed)) throw new SetupError("Choose a recorded failed installation with owned resources remaining. No installation will be resumed.");
    if (receipt.setupPid) {
      let active = true;
      try { process.kill(receipt.setupPid, 0); }
      catch (error) {
        if ((error as NodeJS.ErrnoException).code === "ESRCH") active = false;
        else throw new SetupError("Cannot verify that the original installer stopped. Close it before reopening cleanup.");
      }
      if (active) throw new SetupError("Close the original installer before reopening its cleanup page.");
    }
    const tools: BetaTools = { kubectl: "kubectl", kind: "kind", helm: "helm", runtime: receipt.runtime, env: { ...process.env } };
    this.consumed = true; this.state.phase = "failed";
    this.recordRecovery(cluster, tools);
    this.state.message = "Reopened a retained failed attempt for inspection and explicitly confirmed cleanup only. Installation cannot be resumed; no resources changed.";
    if (receipt.phase !== "deleted") {
      this.observation = observeSetupCluster(tools, cluster, path.join(betaDirectory(cluster, this.deps.root), "kubeconfig"),
        this.deps.execute, observation => { this.state.observation = observation; });
      await this.observation.sample(); this.observation.stop(); this.observation = undefined;
    }
  }

  private recordRecovery(cluster: string, tools: BetaTools): void {
    const directory = betaDirectory(cluster, this.deps.root);
    this.state.recovery = { clusterName: cluster, directory, workingDirectory: this.deps.root, receipt: "absent" };
    try {
      const receipt = readBetaState(directory);
      if (!receipt) return;
      for (const code of receipt.failure?.coreHookFailures ?? []) {
        const id = `core-hook-${code}`;
        if (!this.state.checks.some(check => check.id === id)) {
          this.state.checks.push({ id, label: "Core hook failure", status: "failed",
            detail: `${coreHookFailureMessage(code)} [${code}]`, diagnosticCode: code });
        }
      }
      if (receipt.nodeId && ["failed", "deleted"].includes(receipt.phase)) this.cleanupTarget = { directory, receipt, tools };
      const remaining = receipt.phase !== "deleted" || receipt.trust?.installed;
      this.state.recovery = { clusterName: cluster, directory, workingDirectory: this.deps.root, receipt: "available",
        nodeRecorded: Boolean(receipt.nodeId && receipt.phase !== "deleted"), hostTrustInstalled: receipt.trust?.installed === true,
        cleanupAvailable: Boolean(this.cleanupTarget && remaining),
        ...(receipt.nodeId && remaining ? { cleanupCommand: setupCleanupCommand(cluster) } : {}) };
    } catch { this.state.recovery.receipt = "unreadable"; }
  }

  private readonly executeAuthorized: BetaExecute = async (tool, args, options) => {
    this.authorizationAbort.signal.throwIfAborted();
    if (path.basename(tool) === "sudo") await this.ensureTerminalAuthorization();
    if (!isTrustAuthorization(tool, args)) return this.deps.execute(tool, args, options);
    return this.withAuthorization("macos", () => this.deps.execute(tool, args, { ...options, cancelSignal: this.authorizationAbort.signal }));
  };

  async cleanup(value: unknown): Promise<void> {
    const input = value as { clusterName?: unknown; confirm?: unknown };
    const target = this.cleanupTarget;
    if (!input || typeof input !== "object" || Array.isArray(input) || Object.keys(input).some(key => !["clusterName", "confirm"].includes(key))
      || input.confirm !== true || !target || input.clusterName !== target.receipt.cluster) throw new SetupError("Explicitly confirm cleanup of this installer's exact failed destination");
    if (this.state.busy || this.state.phase !== "failed" || this.state.cleanup?.status === "passed") throw new SetupError("Cleanup is available only for this completed failed attempt");
    const verify = () => {
      const receipt = readBetaState(target.directory);
      if (!receipt || receipt.owner !== target.receipt.owner || receipt.nodeId !== target.receipt.nodeId
        || receipt.endpoint !== target.receipt.endpoint || receipt.runtime !== target.receipt.runtime
        || receipt.sshKey !== target.receipt.sshKey || !["failed", "deleted"].includes(receipt.phase)
        || JSON.stringify(receipt.trust && { ...receipt.trust, installed: undefined }) !== JSON.stringify(target.receipt.trust && { ...target.receipt.trust, installed: undefined })) {
        throw new SetupError("Ownership changed or setup is still active. Nothing further was removed; inspect the private receipt.");
      }
      return receipt;
    };
    verify();
    this.state.busy = true;
    this.state.cleanup = { status: "running", detail: "Checking recorded ownership and removing only this attempt's node and host trust. Private receipts will remain." };
    delete this.state.authorization; delete this.state.authorizationTarget;
    try {
      if (verify().trust?.installed) await this.ensureTerminalAuthorization();
      verify();
      await teardownManagedBeta(target.directory, target.tools, this.executeAuthorized,
        receipt => changeManagedHostTrust(receipt, false, this.executeAuthorized));
      const receipt = verify();
      if (receipt.phase !== "deleted" || receipt.trust?.installed) throw new SetupError("Cleanup did not finish; retained receipts record the remaining resources.");
      this.state.cleanup = { status: "passed", detail: "Owned node, its contained data and recorded host trust removed. Private failure receipts retained. This installation identity remains consumed." };
      this.state.recovery!.nodeRecorded = false; this.state.recovery!.hostTrustInstalled = false;
      delete this.state.recovery!.cleanupCommand;
      this.state.recovery!.cleanupAvailable = false;
    } catch (error) {
      this.state.cleanup = { status: "failed", detail: error instanceof SetupError ? error.message
        : "Cleanup could not complete. It may be partial; private ownership receipts were retained. Inspect ownership and authorization before retrying cleanup. No install was retried." };
      try {
        const receipt = verify();
        this.state.recovery!.nodeRecorded = receipt.phase !== "deleted";
        this.state.recovery!.hostTrustInstalled = receipt.trust?.installed === true;
      } catch {
        this.state.recovery!.receipt = "unreadable"; this.state.recovery!.cleanupAvailable = false;
        this.state.cleanup.detail = "Ownership could not be revalidated after partial cleanup. Nothing further will be removed; inspect the private receipt.";
      }
    } finally { this.state.busy = false; }
  }

  async initialize(): Promise<void> { await this.contexts(); }
  private async contexts(): Promise<void> {
    try {
      this.inventory = JSON.parse((await this.read("kubectl", ["config", "view", "-o", "json"])).stdout);
      this.state.contexts = (this.inventory.contexts ?? []).filter((item: any) => safeText(item.name, 253) && !item.name.startsWith("-") && safeText(item.context?.cluster, 253))
        .slice(0, 100).map((item: any) => ({ name: item.name, cluster: item.context.cluster, current: item.name === this.inventory["current-context"] }));
    } catch { this.inventory = undefined; this.state.contexts = []; }
  }
  private read(tool: string, args: string[], env?: NodeJS.ProcessEnv) {
    return this.deps.execute(tool, args, { env, stdio: "pipe" });
  }
  private async withAuthorization<T>(target: "terminal" | "macos", work: () => Promise<T>): Promise<T> {
    this.state.authorizationTarget = target;
    this.state.authorization = "waiting";
    try {
      const result = await work();
      this.authorizationAbort.signal.throwIfAborted();
      this.state.authorization = "approved";
      return result;
    } catch (error) {
      this.state.authorization = "failed";
      throw error;
    }
  }
  private async ensureTerminalAuthorization(): Promise<void> {
    this.authorizationAbort.signal.throwIfAborted();
    try { await this.read("sudo", ["-n", "true"]); }
    catch {
      this.authorizationAbort.signal.throwIfAborted();
      await this.withAuthorization("terminal", async () => {
        await this.deps.authorizeTerminal(this.authorizationAbort.signal);
        this.authorizationAbort.signal.throwIfAborted();
        await this.read("sudo", ["-n", "true"]);
      });
    }
    this.authorizationAbort.signal.throwIfAborted();
  }
  private resolve(value: unknown): SetupDraft {
    const draft = parseSetupDraft(value);
    if (draft.target === "kind" && draft.provider.kind === "github-copilot") {
      if (draft.provider.apiKey) throw new SetupError("Use GitHub sign-in rather than pasting a token");
      draft.provider.apiKey = this.copilot.credential(draft.provider.model);
    }
    return draft;
  }
  private async check(id: string, label: string, work: () => Promise<string>): Promise<boolean> {
    const check = { id, label, status: "running" as const };
    this.state.checks.push(check);
    try { Object.assign(check, { status: "passed", detail: await work() }); return true; }
    catch (error) { Object.assign(check, { status: "blocked", detail: error instanceof SetupError || error instanceof KindInventoryError ? error.message : `${label} could not be verified; no installation was started` }); return false; }
  }
  private async localTools(): Promise<BetaTools | undefined> {
    const tools: BetaTools = { kind: "kind", kubectl: "kubectl", helm: "helm", runtime: "podman", env: { ...process.env, KIND_EXPERIMENTAL_PROVIDER: "podman" } };
    let ready = true, podmanReady = false;
    for (const tool of ["kind", "podman", "kubectl", "helm", "openssl", "sudo"] as const) {
      const passed = await this.check(`tool-${tool}`, tool === "sudo" ? "Terminal authorization tool" : tool, async () => {
        let resolved: string;
        try { resolved = (await this.read("which", [tool])).stdout.trim(); }
        catch { throw new SetupError(`${tool} was not found on the installer's PATH. Install it using its official instructions in the terminal, then retry. Setup does not install host tools automatically.`); }
        if (!path.isAbsolute(resolved) || !safeText(resolved)) throw new SetupError(`${tool} was not found on the installer's PATH. Install the tool in the terminal, then retry.`);
        if (tool === "podman") tools.runtime = resolved;
        else if (tool === "kind" || tool === "kubectl" || tool === "helm") tools[tool] = resolved;
        const args = tool === "helm" ? ["version", "--short"] : tool === "kubectl" ? ["version", "--client", "-o", "json"]
          : tool === "openssl" ? ["version"] : ["--version"];
        let version: string;
        try { version = (await this.read(resolved, args)).stdout; }
        catch { throw new SetupError(`${tool} is installed but its version check failed. Run ${tool} ${args.join(" ")} in the launching terminal to diagnose it, then retry.`); }
        if (tool === "kind") {
          const match = version.match(/v?(\d+)\.(\d+)\.(\d+)/);
          if (!match || (Number(match[1]) === 0 && Number(match[2]) < 20)) throw new SetupError("Kind v0.20 or newer is required. Update Kind in the terminal, then retry.");
        }
        if (tool === "helm" && !/^v4\.1\.3(?:\+[^\s]+)?\s*$/.test(version)) throw new SetupError("Maintained Kind beta hooks require Helm 4.1.3. Select that version in the launching terminal, then retry.");
        return tool === "sudo" ? "Available. No privilege was requested; host-trust authorization is checked again before installation."
          : `${tool} is available and its version command succeeded.${tool === "helm" ? " Required version: 4.1.3." : ""}`;
      });
      if (tool === "podman") podmanReady = passed;
      ready = passed && ready;
    }
    const runtimeReady = await this.check("runtime", "Running local Podman service", async () => {
      if (!podmanReady) throw new SetupError("Install or repair Podman first, then retry to check its selected local service.");
      await this.localConnection("prerequisite-check", tools);
      return "Selected local Podman service responds. No containers or clusters were created.";
    });
    ready = ready && runtimeReady;
    const architectureReady = await this.check("architecture", "Schema image and node architecture", async () => {
      if (!runtimeReady) throw new SetupError("Start the selected Podman service before checking its architecture.");
      let architecture: "amd64" | "arm64";
      try { architecture = await this.deps.architecture(tools, this.deps.execute); }
      catch { throw new SetupError("Could not verify a matching Linux platform in the pinned schema image. Check registry access and Podman architecture, and use a matching qualified image. No cluster or host trust was created."); }
      return `The pinned schema image supports linux/${architecture}, matching the selected Podman machine. Installation will select the verified node architecture without changing image digests or node labels.`;
    });
    ready = ready && architectureReady;
    return ready ? tools : undefined;
  }
  private async localConnection(clusterName: string, tools: BetaTools): Promise<Awaited<ReturnType<typeof beginBetaState>>> {
    let connection: Awaited<ReturnType<typeof beginBetaState>>;
    try { connection = await beginBetaState(clusterName, tools, this.deps.execute); }
    catch { throw new SetupError("Select one local Podman connection. Run podman system connection list in the launching terminal and check CONTAINER_HOST / CONTAINER_CONNECTION overrides. Remote Podman hosts cannot serve this local workspace."); }
    Object.assign(tools, pinnedBetaTools(tools, connection));
    try { await this.read(tools.runtime, ["info", "--format", "json"], tools.env); }
    catch { throw new SetupError("The selected local Podman service is unavailable. Check podman machine list and podman info in the launching terminal; start your selected machine if needed, then retry."); }
    return connection;
  }
  async prerequisites(value: unknown = {}): Promise<SetupState> {
    if (this.state.busy || this.consumed) throw new SetupError("Cannot check prerequisites during or after an installation");
    this.approved = undefined; this.state.prerequisitesReady = false;
    delete this.state.prerequisiteSelection;
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new SetupError("Invalid prerequisite inputs");
    const input = value as { bridge?: unknown; ports?: unknown };
    if (input.bridge !== undefined && typeof input.bridge !== "boolean") throw new SetupError("Choose whether to include Bridge");
    const bridge = input.bridge !== false;
    let ports: SetupDraft["ports"];
    try { ports = localBetaPorts(input.ports, bridge); }
    catch (error) { throw new SetupError(error instanceof Error ? error.message : "Invalid local ports"); }
    this.state.prerequisiteSelection = { bridge, ports };
    this.state.phase = "prerequisites"; this.state.busy = true; this.state.checks = [];
    delete this.state.authorization; delete this.state.authorizationTarget;
    delete this.state.message;
    try {
      for (const [service, label] of [["https", "Sign-in HTTPS"], ["bridge", "Bridge"], ["headlamp", "Headlamp"]] as const) {
        if (service === "bridge" && !bridge) continue;
        await this.check(`port-${service}`, `${label} port ${ports[service]}`, async () => {
          try { await this.deps.port(ports[service]); }
          catch { throw new SetupError(`Loopback port ${ports[service]} is busy or cannot be bound. Choose another ${label} port under Local ports and check again. Setup will not stop the process using it.`); }
          return `127.0.0.1:${ports[service]} is available now. The check releases the port; availability is checked again before installation.`;
        });
      }
      const tools = await this.localTools();
      await this.check("assets", "Packaged release assets", async () => {
        try { this.deps.assets(); } catch { throw new SetupError("Digest-bound packaged release assets are missing or invalid. Use the prepared release package; source charts are not a safe fallback."); }
        return "Digest-bound packaged Core, Bridge and identity assets available.";
      });
      await this.check("host", "Supported local host", async () => {
        this.deps.hostSupport(); return "Managed local host-trust workflow supported.";
      });
      this.state.prerequisitesReady = Boolean(tools) && this.state.checks.every(check => check.status === "passed");
      this.state.phase = this.state.prerequisitesReady ? "draft" : "blocked";
      this.state.message = this.state.prerequisitesReady ? "Tools and selected ports passed. Continue to inference. These checks run again before installation."
        : "Resolve the blocked checks below: change an unavailable port under Local ports or repair the tool in your terminal, then check again. No installation was started.";
      return this.state;
    } finally { this.state.busy = false; }
  }
  async preflight(value: unknown): Promise<SetupState> {
    if (this.state.busy) throw new SetupError("Another setup operation is running");
    if (this.consumed) throw new SetupError("This install attempt was consumed. Inspect retained evidence, then start a new installer with a fresh cluster name");
    this.approved = undefined;
    this.state.phase = "draft"; this.state.checks = []; this.state.steps = [];
    delete this.state.authorization; delete this.state.authorizationTarget;
    delete this.state.message; delete this.state.handoffUrl; delete this.state.core;
    this.state.providerConfigured = false;
    const draft = this.resolve(value);
    this.state.phase = "preflight"; this.state.busy = true;
    try {
      if (draft.target === "existing") {
        await this.contexts();
        const authenticated = await this.check("context", "Authenticated AKS context and TLS", async () => {
          const selected = this.inventory?.contexts?.find((item: any) => item.name === draft.context);
          const cluster = this.inventory?.clusters?.find((item: any) => item.name === selected?.context?.cluster)?.cluster;
          let url: URL; try { url = new URL(cluster?.server); } catch { throw new SetupError("Selected context is unavailable"); }
          if (url.protocol !== "https:" || url.username || url.password || cluster["insecure-skip-tls-verify"] || cluster["proxy-url"]
            || !/(?:^|\.)azmk8s\.io$/.test(url.hostname)) throw new SetupError("Remote setup requires a verified HTTPS AKS kubecontext without TLS bypass or proxy overrides");
          await this.read("kubectl", ["--context", draft.context, "--request-timeout=15s", "get", "--raw", "/readyz"]);
          return "AKS API accessible with existing credentials; no default context changed.";
        });
        if (authenticated) await this.check("core", "Existing Core and available inference defaults", async () => {
          const core = await inspectSetupCore(draft.context, this.deps.execute);
          this.state.core = core;
          if (core.status === "absent") return "No Core namespace found. This is a fresh target, not an existing workspace to adopt.";
          if (core.status === "unmanaged") throw new SetupError("The Core namespace exists, but an owned Core Helm controller was not verified. Namespace presence alone does not prove an installed Core; existing resources will not be overwritten.");
          return `Existing Core detected: ${core.chart}. ${core.provider && core.model
            ? `Existing Core defaults: ${core.provider} / ${core.model}.`
            : "No explicit Core provider/model defaults were found; setup will not invent a replacement."} Credentials stay in the cluster and were not inspected. Provider connectivity, Bridge enrollment and existing administrator access still require verification; no adoption or upgrade was performed.`;
        });
        this.state.checks.push({ id: "remote-inputs", label: "Reviewed remote installation inputs", status: "blocked",
          detail: "AKS browser installation is not supported yet: this form has no reviewed Entra/OIDC application, callback/TLS, workload-identity and Core/Bridge enrollment values. Use the maintained CLI/Helm flow; existing workspace authority cannot be replaced by local Dex or submitted keys." });
      } else {
        await this.check("assets", "Packaged release assets", async () => { this.deps.assets(); return draft.bridge
          ? "Digest-bound packaged Core, Bridge and Dex assets available; no dirty-checkout fallback."
          : "Digest-bound packaged Core and Dex assets available. Dex uses the packaged identity template; the Bridge release will not be installed. No dirty-checkout fallback."; });
        const tools = await this.localTools();
        let connection: Awaited<ReturnType<typeof beginBetaState>> | undefined;
        if (tools) await this.check("target", "Fresh local Podman target", async () => {
          if (existsSync(betaDirectory(draft.clusterName, this.deps.root))) throw new SetupError("Retained attempt exists for this name. Inspect it and choose a fresh name; identities are never replayed");
          connection = await this.localConnection(draft.clusterName, tools);
          await assertFreshKindBetaCluster(draft.clusterName, tools, this.deps.execute);
          return `Create one fresh Kind cluster '${draft.clusterName}' on the selected local Podman connection with a private kubeconfig; current context is preserved.`;
        });
        await this.check("trust", "Host trust and local access prerequisites", async () => {
          this.deps.hostSupport();
          if (!tools) throw new SetupError("Resolve the blocked prerequisite tools before authorizing host access.");
          const ports = draft.bridge ? Object.values(draft.ports) : [draft.ports.https, draft.ports.headlamp];
          for (const port of ports) {
            try { await this.deps.port(port); }
            catch { throw new SetupError(`Loopback port ${port} is unavailable. Choose another port under Local ports, then run the setup check again.`); }
          }
          await this.ensureTerminalAuthorization();
          return `Ports ${ports.join(", ")} available. Installation requires separate consent for one owned /etc/hosts entry and a hostname-constrained one-year CA; OS trust-store permission may still be required.`;
        });
        await this.check("provider", "Inference and first sign-in inputs", async () => {
          validateProvider(draft.provider);
          if (!draft.provider.apiKey?.trim()) throw new SetupError("Inference API key is required");
          return "Provider endpoint and credential inputs validated; key stored only in private bootstrap Secret inputs, password bcrypt-hashed for real Dex. Provider inference and browser sign-in remain unverified until installed use.";
        });
        if (connection && this.state.checks.every(check => check.status === "passed")) {
          await this.check("inference", "Model responds through the chosen provider", async () => {
            await this.deps.verifyProvider(draft.provider);
            return "The provider returned a model reply. This billable connection check is not an installed SRE/orchestrator workflow acceptance test.";
          });
        }
        if (tools && connection && this.state.checks.every(check => check.status === "passed")) {
          this.approved = { hash: fingerprint(draft), tools, endpoint: connection.endpoint, sshKey: connection.sshKey };
        }
      }
      this.state.phase = this.approved ? "review" : "blocked";
      this.state.message = this.approved
        ? `Review: create fresh Kind, Cilium, local Dex, ${draft.bridge ? "Core and Bridge Helm releases, OIDC RBAC, provider Secrets and governed workspace activation" : "Core with Headlamp, read-only OIDC access and governed provider setup; no Bridge release or Bridge administrator"}. Nothing has been installed. Confirm this exact target and host-trust changes to continue.`
        : "Preflight blocked. No cluster or host-trust changes were made.";
      return this.state;
    } finally { this.state.busy = false; draft.admin.password = ""; draft.provider.apiKey = ""; }
  }
  async install(value: unknown): Promise<void> {
    if (this.state.busy || this.consumed) throw new SetupError("An install is already running or this attempt was consumed");
    const draft = this.resolve(value), approved = this.approved;
    if (!approved || fingerprint(draft) !== approved.hash || !draft.consent.target || !draft.consent.hostTrust) throw new SetupError("Run preflight for these exact inputs and explicitly consent to the target and host-trust changes");
    // Invalidate the approval before asynchronous work; never replay a partially consumed identity.
    this.approved = undefined; this.consumed = true; this.state.busy = true; this.state.phase = "installing";
    delete this.state.authorization; delete this.state.authorizationTarget;
    this.state.installation = { clusterName: draft.clusterName, bridge: draft.bridge, ports: { ...draft.ports }, startedAt: Date.now() };
    const stages = [
      ["identity", "Prepare identity", "Generate private TLS certificates, hash your account password and prepare provider inputs."],
      ["cluster", "Create Kind cluster", "Start the pinned node image on Podman and verify Kubernetes and the OIDC configuration. Image preparation may take several minutes."],
      ["network", "Configure networking", "Install Cilium and wait for the node and network policies to become ready."],
      ["dex", "Set up secure sign-in", "Start Dex, configure DNS and host trust, then verify HTTPS discovery. Approve any hostname-specific macOS certificate-trust dialog; enter your OS password only in that system dialog, never in this browser."],
      ["core", "Install Core", "Install the Core Helm release and wait for the SRE runtime and registration. Helm hooks may take several minutes."],
      ...(draft.bridge ? [["bridge", "Install Bridge", "Install the visual workspace and wait for its services and enrollment hooks."]] : []),
      ["providers", "Activate inference", "Enroll governed provider credentials and configure the installed workspace."],
      ["access", "Open local access", "Start loopback-only tunnels and verify the UI responds before handing over."],
    ];
    this.state.steps = stages.map(([id, label, detail]) => ({ id, label, detail, status: "pending" }));
    this.state.steps[0].status = "running"; this.state.steps[0].startedAt = Date.now();
    this.state.message = "Installation started. Keep the terminal open; progress below follows the actual installation stages.";
    let active = "identity";
    const azure = draft.provider.kind === "foundry" || draft.provider.kind === "azure-openai";
    const credentials: KarsConfig | null = azure ? { provider: "foundry", endpoint: draft.provider.endpoint, model: draft.provider.model, apiKey: draft.provider.apiKey! } : null;
    const providerConnection = azure ? undefined : { ...draft.provider, kind: draft.provider.kind as "openai" | "anthropic" | "github-copilot" | "openai-compatible" };
    try {
      this.deps.assets();
      await this.deps.install(draft.clusterName, approved.tools, { ensureCluster, installLocalCoreChart,
        openBrowser: async url => {
          if (url !== `http://localhost:${draft.bridge ? draft.ports.bridge : draft.ports.headlamp}`) throw new SetupError("Unexpected local handoff URL");
          this.observation?.stop(); this.observation = undefined;
          this.state.steps.forEach(step => { step.status = "passed"; step.finishedAt ??= Date.now(); });
          this.state.installation!.finishedAt = Date.now();
          this.state.phase = "ready"; this.state.busy = false; this.state.handoffUrl = url;
          this.state.message = "Installation and HTTP readiness checks passed. Sign in with your new Dex account. Customer workflows and inference have not yet been acceptance-tested. Installer shutdown preserves these access tunnels; Ctrl+C closes tunnels, never the cluster.";
        } }, { includeBridge: draft.bridge, ports: draft.ports, account: draft.admin, provider: draft.provider.kind, providerConnection, expectedRuntime: approved,
        inference: { load: () => credentials, interactive: false, prompt: async () => { throw new SetupError("Browser setup never prompts for credentials in the terminal"); } },
        execute: this.executeAuthorized, progress: (stage, detail) => {
          const previous = this.state.steps.find(step => step.id === active);
          if (previous && active !== stage) { previous.status = "passed"; previous.finishedAt = Date.now(); }
          active = stage;
          const step = this.state.steps.find(step => step.id === stage)!;
          step.status = "running"; step.startedAt ??= Date.now();
          if (detail) step.detail = detail;
          if (stage === "core" && !this.observation && !this.authorizationAbort.signal.aborted) {
            this.observation = observeSetupCluster(approved.tools, draft.clusterName,
              path.join(betaDirectory(draft.clusterName, this.deps.root), "kubeconfig"), this.deps.execute,
              observation => { this.state.observation = observation; });
          }
        } });
      if ((this.state.phase as SetupState["phase"]) === "ready") { this.state.phase = "closed"; delete this.state.handoffUrl; this.state.message = "Access tunnels closed. Cluster, identities and private evidence are retained."; }
      else throw new SetupError("Access ended before readiness was verified; inspect retained installation evidence");
    } catch {
      await this.observation?.sample();
      this.observation?.stop(); this.observation = undefined;
      this.state.phase = "failed"; delete this.state.handoffUrl;
      const step = this.state.steps.find(step => step.id === active);
      if (step) { step.status = "failed"; step.finishedAt = Date.now(); }
      this.state.installation!.finishedAt = Date.now();
      const failures = [...new Set((this.state.observation?.resources ?? []).map(row => row.diagnosticCode).filter(isCoreHookFailureCode))];
      if (failures.length) {
        try {
          const directory = betaDirectory(draft.clusterName, this.deps.root);
          const receipt = readBetaState(directory);
          if (!receipt || receipt.phase !== "failed") throw new Error("No failed ownership receipt");
          receipt.failure = { at: new Date().toISOString(), message: "Installation failed; retained Core hook diagnostics.",
            ...receipt.failure, coreHookFailures: failures };
          writeBetaState(directory, receipt);
        } catch {
          this.state.checks.push({ id: "failure-evidence", label: "Retain failure diagnosis", status: "blocked",
            detail: "Could not save the hook diagnosis to the private receipt. Keep this page and the failed node open for inspection before cleanup." });
        }
      }
      this.recordRecovery(draft.clusterName, approved.tools);
      this.state.message = `${step?.label ?? "Installation"} failed. No automatic rollback or deletion ran. This attempt cannot be replayed. Inspect the retained state below before starting a new installer with a fresh cluster name.`;
    } finally {
      this.state.busy = false; draft.admin.password = ""; draft.provider.apiKey = "";
      if (credentials) credentials.apiKey = "";
      if (providerConnection) providerConnection.apiKey = "";
      this.copilot.cancel();
    }
  }
}
