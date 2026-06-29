// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { blockedCommand } from "./egress/blocked.js";
import {
  allowExtraCommand,
  approvalsCommand,
  revokeCommand,
} from "./egress/approval.js";
import {
  EGRESS_ALLOWLIST_MEDIA_TYPE,
  autoDetectSignMode,
  buildCanonicalAllowlist,
  buildEmitManifestYaml,
  describeSignerIdentity,
  ensureSigningTools,
  patchKarsSandbox,
  pushArtifact,
  readKarsSandboxState,
  signArtifact,
  writeEmitManifest,
} from "./egress/sign.js";

/** A baseline allowlist endpoint we ADD: host plus an explicit port (the signed
 *  allowlist requires a concrete port — the canonical builder rejects 0). */
export interface BaselineEndpoint {
  host: string;
  port: number;
}

/** An endpoint as it appears in the live CR: the port may be absent (the CRD
 *  permits a port-less host even though the signer later requires one). Read
 *  and patched verbatim so we never silently drop an existing entry. */
export interface RawEndpoint {
  host: string;
  port?: number;
}

/** Parse a `--approve`/`--deny` domain argument of the form `host` or
 *  `host:port`. Defaults the port to 443 (HTTPS) when omitted — the common
 *  case for agent egress, and the value the signed baseline requires. Throws
 *  on an empty host or an out-of-range port. */
export function parseDomainPort(raw: string, defaultPort = 443): BaselineEndpoint {
  const trimmed = (raw ?? "").trim();
  if (!trimmed) throw new Error("domain must not be empty");
  // Reject a scheme (https://…) — callers pass a bare host[:port].
  if (trimmed.includes("/")) {
    throw new Error(`domain must be a bare host[:port], not a URL: ${raw}`);
  }
  const lastColon = trimmed.lastIndexOf(":");
  let host = trimmed;
  let port = defaultPort;
  if (lastColon > 0) {
    const maybePort = trimmed.slice(lastColon + 1);
    if (/^\d+$/.test(maybePort)) {
      host = trimmed.slice(0, lastColon);
      port = Number(maybePort);
    }
  }
  host = host.trim().toLowerCase();
  if (!host) throw new Error(`domain must not be empty: ${raw}`);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`port out of range [1,65535]: ${port}`);
  }
  return { host, port };
}

/** Add `ep` to `endpoints` if not already present (host+port match), returning
 *  a new sorted array that preserves every existing entry verbatim (including
 *  port-less ones). Pure — used to build the merge-patch payload. */
export function unionEndpoint(
  endpoints: RawEndpoint[],
  ep: BaselineEndpoint,
): RawEndpoint[] {
  const exists = endpoints.some(
    (e) => e.host.toLowerCase() === ep.host && (e.port ?? 443) === ep.port,
  );
  const next = exists ? endpoints.slice() : [...endpoints, ep];
  return sortEndpoints(next);
}

/** Remove every entry whose host matches `host` (any port), returning a new
 *  sorted array. Pure. */
export function removeHost(
  endpoints: RawEndpoint[],
  host: string,
): RawEndpoint[] {
  const h = host.trim().toLowerCase();
  return sortEndpoints(endpoints.filter((e) => e.host.toLowerCase() !== h));
}

function sortEndpoints(eps: RawEndpoint[]): RawEndpoint[] {
  return eps
    .slice()
    .sort((a, b) => (a.host < b.host ? -1 : a.host > b.host ? 1 : (a.port ?? 0) - (b.port ?? 0)));
}

export function egressCommand(): Command {
  const cmd = new Command("egress");
  // Slice 5a — `kars egress blocked <sandbox>` subcommand surfaces
  // the router's /internal/egress/blocked view. Kept as a subcommand
  // rather than another top-level flag so --watch/--top/--since don't
  // collide with the existing flat-options surface.
  cmd.addCommand(blockedCommand());
  // Slice 5e — TTL-scoped, audit-logged grants on top of the signed
  // baseline. Three subcommands so flags don't collide with the legacy
  // `--approve <domain>` learn-mode surface, which is allowlist
  // mutation, not approval-CR mutation.
  cmd.addCommand(allowExtraCommand());
  cmd.addCommand(approvalsCommand());
  cmd.addCommand(revokeCommand());

  cmd
    .description("Manage network egress: allowlist, approvals, and learn mode")
    .argument("[name]", "Sandbox name (default: demo-agent)", "demo-agent")
    .option("--namespace <ns>", "Kubernetes namespace")
    .option("--learn", "Enable learn mode (log all accessed domains)")
    .option("--no-learn", "Disable learn mode")
    .option("--learned", "Show domains discovered during learn mode")
    .option("--pending", "Show learned domains not yet in the allowlist (candidates to approve)")
    .option("--approve <domain[:port]>", "Add a domain to the sealed baseline allowlist (default port 443) and re-sign")
    .option("--deny <domain>", "Remove a domain from the baseline allowlist and re-sign")
    .option("--allowlist", "Show currently approved domains")
    .option("--enforce", "Seal: switch the sandbox to Strict egress mode and sign the current baseline allowlist")
    .option("--status", "Show blocklist and learn mode status")
    .option("--sign", "Build canonical allowlist artifact, push to OCI registry, sign with cosign, patch allowlistRef. **Default-on** when combined with --enforce or --approve. Pass --no-sign to opt out.")
    .option("--no-sign", "Skip signing. The controller will refuse to use the artifact in authoritative mode (SignerPolicyMissing). Use only for local dev.")
    .option("--sign-mode <mode>", "Cosign mode: keyless | identity-token | keyed (default: auto-detect)")
    .option("--sign-key <ref>", "Cosign key reference (path or KMS URI like azurekms://...) — required for --sign-mode keyed")
    .option("--registry <fqdn>", "Override target ACR for the artifact push (default: auto-discover)")
    .option("--repository <repo>", "Repository path within the registry (default: policy/egress-allowlist/<sandbox>)")
    .option("--emit-manifest <path>", "GitOps mode: write the KarsSandbox patch to <path> instead of running 'kubectl patch'. Requires signing (default-on). Refuses to overwrite without --force.")
    .option("--force", "With --emit-manifest, overwrite an existing file.")
    .action(async (name: string, options) => {
      const { execa } = await import("execa");

      // S12.g — sign-by-default. When the operator mutates the baseline
      // allowlist (--approve / --deny) or seals the sandbox (--enforce),
      // signing happens automatically unless --no-sign is passed. options.sign:
      //   - undefined → not specified → default to true in signing context
      //   - true      → user passed --sign explicitly
      //   - false     → user passed --no-sign
      // --deny MUST be in the signing context: a removal that is not re-signed
      // leaves the old signed bundle authoritative, so the "denied" host would
      // still be served (a fail-open revocation).
      const inSigningContext = Boolean(options.enforce || options.approve || options.deny);
      const signRequested =
        options.sign === false ? false : (options.sign === true || inSigningContext);

      // --emit-manifest implies a signing context; require --enforce/--approve/--deny.
      if (options.emitManifest && !inSigningContext) {
        console.log(
          chalk.red(
            `\n  --emit-manifest requires --enforce, --approve, or --deny (the artifact is built from the live allowlist).\n`,
          ),
        );
        process.exitCode = 1;
        return;
      }

      // --emit-manifest with --no-sign is a contradiction. GitOps mode
      // promotes the artifact off-cluster; an unsigned artifact would
      // fail authoritative-mode verify on the cluster with no
      // operator present to retry. Refuse loud.
      if (options.emitManifest && options.sign === false) {
        console.log(
          chalk.red(
            `\n  --emit-manifest cannot be combined with --no-sign — GitOps mode requires signed artifacts.\n`,
          ),
        );
        process.exitCode = 1;
        return;
      }

      // Legacy guard: --sign without --enforce/--approve is still a hard
      // error (sign-by-default only applies inside a signing context).
      if (options.sign === true && !inSigningContext) {
        console.log(chalk.red(`\n  --sign requires --enforce, --approve, or --deny.\n`));
        process.exitCode = 1;
        return;
      }

      // Loud warning when the user opts out of default-on signing.
      if (inSigningContext && options.sign === false) {
        console.log(
          chalk.yellow(
            `\n  ⚠ --no-sign: the resulting allowlist will be unsigned. The controller will emit AllowlistVerified=False/SignerPolicyMissing and refuse the artifact in authoritative mode. Use only for local dev.\n`,
          ),
        );
      }

      const containerName = `kars-${name}`;
      const ns = options.namespace || containerName;

      // Detect whether this is a local Docker container or a Kubernetes pod
      let mode: "docker" | "k8s" = "k8s";
      let pod = "";
      try {
        const { stdout } = await execa("docker", [
          "inspect", "--format", "{{.State.Running}}", containerName,
        ], { stdio: "pipe" });
        if (stdout.trim() === "true") mode = "docker";
      } catch {
        // No local container — try Kubernetes
      }

      if (mode === "k8s") {
        try {
          const { stdout } = await execa("kubectl", [
            "get", "pods", "-n", ns,
            "-o", `jsonpath={.items[?(@.status.phase=="Running")].metadata.name}`,
          ], { stdio: "pipe" });
          pod = stdout.trim().split(/\s+/)[0];
          if (!pod) throw new Error("no pod");
        } catch {
          console.log(chalk.red(`\n  No running sandbox found for '${name}' (checked Docker and AKS).\n`));
          return;
        }
      }

      // Read admin token for authenticated router calls (AKS only)

      // Helper: call router API — Docker exec or kubectl exec
      async function routerGet(path: string): Promise<any> {
        let curlArgs = mode === "docker"
          ? ["exec", containerName, "curl", "-s", `http://127.0.0.1:8443${path}`]
          : ["exec", "-n", ns, pod, "-c", "inference-router", "--",
             "/usr/local/bin/kars-inference-router", "probe", path];
        const bin = mode === "docker" ? "docker" : "kubectl";
        const { stdout } = await execa(bin, curlArgs, { stdio: "pipe" });
        return JSON.parse(stdout);
      }

      async function routerPost(path: string, body: object): Promise<any> {
        let curlArgs = mode === "docker"
          ? ["exec", containerName, "curl", "-s", "-X", "POST",
             "-H", "Content-Type: application/json",
             "-d", JSON.stringify(body),
             `http://127.0.0.1:8443${path}`]
          : ["exec", "-n", ns, pod, "-c", "inference-router", "--",
             "/usr/local/bin/kars-inference-router", "probe", "POST", path, JSON.stringify(body)];
        const bin = mode === "docker" ? "docker" : "kubectl";
        const { stdout } = await execa(bin, curlArgs, { stdio: "pipe" });
        return JSON.parse(stdout);
      }

      // The baseline-mutating + sealing operations require the KarsSandbox CRD
      // and the controller's signing pipeline, neither of which exists for a
      // local Docker sandbox. Refuse clearly rather than failing deep in a
      // kubectl call.
      if (mode === "docker" && (options.approve || options.deny || options.enforce)) {
        console.log(chalk.red(
          `\n  --approve / --deny / --enforce operate on the KarsSandbox CRD and the signed allowlist,\n` +
          `  which only exist on a Kubernetes-deployed sandbox (kars up / kars add).\n` +
          `  For local Docker dev, use --learn / --learned to observe egress.\n`,
        ));
        return;
      }

      // Approve a domain — add it to the sealed baseline allowlist and re-sign.
      // Slice 5c.1 removed the in-process /egress/approve side door; the
      // allowlist is now the controller-published, cosign-verified bundle built
      // from KarsSandbox.spec.networkPolicy.allowedEndpoints. So "approve" means
      // "add to that baseline (host+port) and re-sign". For a temporary,
      // TTL-scoped grant instead, use `kars egress allow-extra`.
      if (options.approve) {
        let ep: BaselineEndpoint;
        try {
          ep = parseDomainPort(options.approve);
        } catch (e: any) {
          console.log(chalk.red(`\n  Invalid --approve value: ${e.message}\n`));
          return;
        }
        try {
          const crNs = await discoverKarsSandboxNamespace(name, ns);
          const endpoints = await readRawAllowedEndpoints(crNs, name);
          const already = endpoints.some((e) => e.host.toLowerCase() === ep.host && (e.port ?? 443) === ep.port);
          const hasPortless = endpoints.some((e) => e.port === undefined);
          const next = unionEndpoint(endpoints, ep);
          // Patch when we add a new endpoint OR when there are port-less entries
          // to normalize (so the about-to-run sign step doesn't silently drop
          // them). If neither, the baseline is already correct and we fall
          // straight through to re-signing (self-healing a prior failed sign).
          if (!already || hasPortless) {
            const { normalized } = await patchBaselineEndpoints(crNs, name, next);
            console.log(already
              ? chalk.dim(`\n  ${ep.host}:${ep.port} already approved for '${name}'.`)
              : chalk.green(`\n  ✅ Approved: ${ep.host}:${ep.port}`));
            if (!already) console.log(chalk.dim(`     Added to the baseline allowlist (${next.length} endpoint(s) total).`));
            if (normalized > 0) {
              console.log(chalk.dim(`     (${normalized} existing port-less entr${normalized === 1 ? "y" : "ies"} defaulted to :443 so the signed baseline stays valid.)`));
            }
          } else {
            console.log(chalk.dim(`\n  ${ep.host}:${ep.port} is already in the baseline allowlist for '${name}' — re-signing to confirm.`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to approve: ${e.message}\n`));
          return;
        }
        if (signRequested) {
          const ok = await runSignFlow(name, ns, options);
          if (!ok) {
            console.log(chalk.yellow(`  ⚠ The baseline was updated but signing did NOT complete — the previously-signed allowlist is still authoritative. Re-run \`kars egress ${name} --approve ${options.approve}\` to finish.\n`));
          }
        } else {
          console.log(chalk.yellow(`\n  ⚠ --no-sign: the controller will not serve this change in authoritative mode until a signed bundle is published.\n`));
        }
        return;
      }

      // Deny a domain — remove it from the baseline allowlist and re-sign.
      if (options.deny) {
        let host: string;
        try {
          host = parseDomainPort(options.deny).host;
        } catch (e: any) {
          console.log(chalk.red(`\n  Invalid --deny value: ${e.message}\n`));
          return;
        }
        try {
          const crNs = await discoverKarsSandboxNamespace(name, ns);
          const endpoints = await readRawAllowedEndpoints(crNs, name);
          const next = removeHost(endpoints, host);
          const changed = next.length !== endpoints.length;
          const hasPortless = endpoints.some((e) => e.port === undefined);
          if (changed || hasPortless) {
            await patchBaselineEndpoints(crNs, name, next);
          }
          if (changed) {
            console.log(chalk.yellow(`\n  ❌ Denied: ${host}`));
            console.log(chalk.dim(`     Removed from the baseline allowlist (${next.length} endpoint(s) remain).`));
          } else {
            // Not in the inline baseline. Still re-sign so a previously-removed
            // host whose revocation never got signed is reconciled now (the
            // signed bundle is the authoritative artifact, not the inline list).
            console.log(chalk.dim(`\n  ${host} is not in the baseline allowlist for '${name}' — re-signing the current baseline to ensure the revocation is authoritative.`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to deny: ${e.message}\n`));
          return;
        }
        if (signRequested) {
          const ok = await runSignFlow(name, ns, options);
          if (!ok) {
            console.log(chalk.yellow(`  ⚠ The host was removed from the baseline but signing did NOT complete — the previously-signed bundle may still allow '${host}'. Re-run \`kars egress ${name} --deny ${host}\` to finish the revocation.\n`));
          }
        } else {
          console.log(chalk.yellow(`\n  ⚠ --no-sign: the removal is NOT yet authoritative — the previously-signed bundle still allows '${host}'. Re-run with --sign (default) to revoke it.\n`));
        }
        return;
      }

      // Enforce: seal the sandbox — switch egress to Strict mode and sign the
      // current baseline. Slice 5c.1 removed /egress/enforce; the authoritative
      // path is the CRD `egressMode` field plus the signed allowlist bundle.
      if (options.enforce) {
        try {
          const crNs = await discoverKarsSandboxNamespace(name, ns);
          await patchEgressMode(crNs, name, "Strict");
          // Normalize any port-less baseline entries to :443 BEFORE signing so
          // the signer (which requires a port) doesn't drop them.
          const endpoints = await readRawAllowedEndpoints(crNs, name);
          if (endpoints.some((e) => e.port === undefined)) {
            const { normalized } = await patchBaselineEndpoints(crNs, name, endpoints);
            if (normalized > 0) {
              console.log(chalk.dim(`     (${normalized} port-less baseline entr${normalized === 1 ? "y" : "ies"} defaulted to :443 for signing.)`));
            }
          }
          // Best-effort live toggle so Strict takes effect without waiting for a
          // pod roll; never let a probe failure block the authoritative patch.
          if (mode === "k8s" && pod) {
            await routerPost("/egress/learn", { enabled: false }).catch(() => {});
          }
          console.log(chalk.green(`\n  🔒 Enforcement (Strict) mode set for '${name}'`));
          console.log(chalk.dim(`     Only allowlisted host:port pairs will pass; the blocklist still applies.`));
          console.log(chalk.dim(`     (The controller may roll the sandbox pod to apply the new mode.)\n`));
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to enforce: ${e.message}\n`));
          return;
        }
        if (signRequested) {
          const ok = await runSignFlow(name, ns, options);
          if (!ok) {
            console.log(chalk.yellow(`  ⚠ Strict mode is set but signing did NOT complete — publish a signed bundle (re-run \`kars egress ${name} --enforce\`) or the controller will refuse the allowlist in authoritative mode.\n`));
          }
        } else {
          console.log(chalk.yellow(`  ⚠ --no-sign: publish a signed bundle (re-run with --sign) before the controller will serve the allowlist in authoritative mode.\n`));
        }
        console.log(chalk.dim(`  Next:`));
        console.log(chalk.dim(`    kars egress ${name} --approve <domain>      Add a domain to the baseline + re-sign`));
        console.log(chalk.dim(`    kars egress allow-extra ${name} --host <h> --ttl PT4H --reason "<why>"   Temporary grant`));
        console.log(chalk.dim(`    kars egress ${name} --learn                 Re-open learn mode\n`));
        return;
      }

      // Show learned domains not yet in the allowlist — the closest analogue to
      // the removed in-memory "pending" queue: domains the agent has tried to
      // reach in learn mode that aren't approved yet.
      if (options.pending) {
        try {
          const [allow, learned] = await Promise.all([
            routerGet("/egress/allowlist").catch(() => ({ domains: [] as string[] })),
            routerGet("/egress/learned").catch(() => ({ domains: [] as string[], learn_mode: false })),
          ]);
          const approved = new Set<string>((allow.domains ?? []).map((d: string) => d.split(":")[0].toLowerCase()));
          const candidates = (learned.domains ?? []).filter((d: string) => !approved.has(d.split(":")[0].toLowerCase()));
          console.log(chalk.hex("#0078D4")(`\n  Egress candidates for '${name}' (learned, not yet approved)`));
          console.log(chalk.dim(`  Learn mode: ${learned.learn_mode ? "ON" : "off"}\n`));
          if (candidates.length > 0) {
            for (const d of candidates.sort()) {
              console.log(`    ${chalk.yellow("⏳")} ${d}`);
              console.log(chalk.dim(`       Approve: kars egress ${name} --approve ${d.split(":")[0]}`));
            }
            console.log(chalk.dim(`\n  ${candidates.length} candidate(s).\n`));
          } else {
            console.log(chalk.dim(`    None. Enable learn mode and exercise the agent to discover endpoints.\n`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to query candidates: ${e.message}\n`));
        }
        return;
      }

      // Show allowlist
      if (options.allowlist) {
        try {
          const data = await routerGet("/egress/allowlist");
          console.log(chalk.hex("#0078D4")(`\n  Egress Allowlist for '${name}'`));
          if (data.domains && data.domains.length > 0) {
            console.log();
            for (const domain of data.domains) {
              console.log(`    ${chalk.green("✓")} ${domain}`);
            }
            console.log(chalk.dim(`\n  ${data.count} domain(s) approved.\n`));
          } else {
            console.log(chalk.dim(`\n    No domains approved yet.\n`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to query allowlist: ${e.message}\n`));
        }
        return;
      }

      // Enable learn mode. In Kubernetes this is durable: patch the CRD
      // egressMode (authoritative, survives a pod roll) then best-effort live
      // toggle. In local Docker dev there is no KarsSandbox CR, so fall back to
      // the runtime-only toggle (original behaviour).
      if (options.learn === true) {
        try {
          if (mode === "k8s") {
            const crNs = await discoverKarsSandboxNamespace(name, ns);
            await patchEgressMode(crNs, name, "Learn");
            await routerPost("/egress/learn", { enabled: true }).catch(() => {});
          } else {
            await routerPost("/egress/learn", { enabled: true });
          }
          console.log(chalk.green(`\n  ✅ Learn mode enabled for '${name}'.`));
          console.log(chalk.dim(`     All accessed domains will be logged (blocklist still enforced).`));
          console.log(chalk.dim(`     Run ${chalk.white(`kars egress ${name} --learned`)} to see discovered domains.\n`));
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to enable learn mode: ${e.message}\n`));
        }
        return;
      }

      // Disable learn mode. In Kubernetes: switch the CRD to Strict (the only
      // non-learn mode) then best-effort live toggle. In Docker: runtime-only.
      if (options.learn === false && process.argv.includes("--no-learn")) {
        try {
          if (mode === "k8s") {
            const crNs = await discoverKarsSandboxNamespace(name, ns);
            await patchEgressMode(crNs, name, "Strict");
            await routerPost("/egress/learn", { enabled: false }).catch(() => {});
            console.log(chalk.yellow(`\n  Learn mode disabled for '${name}' (egress mode is now Strict).`));
            console.log(chalk.dim(`     Only allowlisted host:port pairs will pass. Seal the baseline with ${chalk.white(`kars egress ${name} --enforce`)} to (re)sign it.\n`));
          } else {
            await routerPost("/egress/learn", { enabled: false });
            console.log(chalk.yellow(`\n  Learn mode disabled for '${name}'.\n`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to disable learn mode: ${e.message}\n`));
        }
        return;
      }

      // Show learned domains
      if (options.learned) {
        try {
          const data = await routerGet("/egress/learned");
          console.log(chalk.hex("#0078D4")(`\n  Learned Domains for '${name}'`));
          console.log(chalk.dim(`  Learn mode: ${data.learn_mode ? "ON" : "OFF"}\n`));
          if (data.domains && data.domains.length > 0) {
            for (const domain of data.domains.sort()) {
              console.log(`    ${chalk.green("●")} ${domain}`);
            }
            console.log(chalk.dim(`\n  ${data.count} domain(s) discovered.\n`));
          } else {
            console.log(chalk.dim(`    No domains learned yet.\n`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to query learned domains: ${e.message}\n`));
        }
        return;
      }

      // Default: show status
      try {
        const [blStatus, allowlist, learned] = await Promise.all([
          routerGet("/blocklist/status"),
          routerGet("/egress/allowlist"),
          routerGet("/egress/learned").catch(() => ({ count: 0, domains: [], learn_mode: false })),
        ]);
        // "Pending" no longer exists as an in-router queue (Slice 5c.1). The
        // useful analogue is learned domains not yet in the allowlist.
        const approved = new Set<string>((allowlist.domains ?? []).map((d: string) => d.split(":")[0].toLowerCase()));
        const candidates = (learned.domains ?? []).filter((d: string) => !approved.has(d.split(":")[0].toLowerCase()));
        console.log(chalk.hex("#0078D4")(`\n  Egress Security — '${name}'`));
        console.log(`    Blocklist:      ${blStatus.enabled ? chalk.green("enabled") : chalk.red("disabled")} (${blStatus.domain_count.toLocaleString()} domains)`);
        console.log(`    Learn mode:     ${blStatus.learn_mode ? chalk.green("ON") : chalk.dim("off")}`);
        console.log(`    Allowlist:      ${chalk.white(allowlist.count)} domain(s) approved`);
        console.log(`    Candidates:     ${candidates.length > 0 ? chalk.yellow(candidates.length + " learned, not yet approved") : chalk.dim("none")}`);
        if (learned.count > 0) {
          console.log(`    Learned:        ${chalk.cyan(learned.count)} domain(s) discovered`);
        }
        console.log();
        if (candidates.length > 0 && blStatus.learn_mode) {
          console.log(chalk.dim(`  Discovered domains not yet approved (learn mode):`));
          for (const d of candidates.sort()) {
            console.log(`    ${chalk.cyan("◉")} ${d}`);
          }
          console.log();
          console.log(chalk.hex("#0078D4")(`  → Approve them, then seal: ${chalk.white(`kars egress ${name} --approve <domain>`)} … then ${chalk.white(`kars egress ${name} --enforce`)}`));
          console.log();
        }
        console.log(chalk.dim(`  Commands:`));
        console.log(chalk.dim(`    kars egress ${name} --approve <domain[:port]>  Add to baseline allowlist + re-sign (default :443)`));
        console.log(chalk.dim(`    kars egress ${name} --deny <domain>            Remove from baseline + re-sign`));
        console.log(chalk.dim(`    kars egress ${name} --enforce                  Seal: Strict mode + sign baseline`));
        console.log(chalk.dim(`    kars egress ${name} --pending                  Show learned, not-yet-approved domains`));
        console.log(chalk.dim(`    kars egress ${name} --allowlist                Show approved domains`));
        console.log(chalk.dim(`    kars egress ${name} --learned                  Show discovered domains`));
        console.log(chalk.dim(`    kars egress allow-extra ${name} --host <h> --ttl PT4H --reason "<why>"   Temporary TTL grant`));
        console.log();
      } catch (e: any) {
        console.log(chalk.red(`\n  Failed to query status: ${e.message}\n`));
      }
    });

  return cmd;
}

/**
 * S12.c — orchestrate the canonical-build → oras push → cosign sign →
 * kubectl patch flow. Fails closed: any error before patch aborts;
 * patch only happens after signing succeeds.
 */
async function runSignFlow(
  name: string,
  ns: string,
  options: any,
): Promise<boolean> {
  const headerSlice = options.emitManifest ? "GitOps mode" : "sign-by-default";
  console.log(chalk.hex("#0078D4")(`\n  Signing egress allowlist artifact for '${name}' (${headerSlice})`));
  try {
    const { orasPath, cosignPath } = await ensureSigningTools();

    // The pod-namespace `kars-<name>` is where the sandbox's pod, NetworkPolicy,
    // and per-sandbox secrets live — but the *KarsSandbox CR* itself is created
    // by the operator in the operator's release namespace (default
    // `kars-system`). Read/patch always need the CR's namespace, NOT the
    // pod ns. Discover it once via cross-ns lookup.
    const crNamespace = await discoverKarsSandboxNamespace(name, ns);

    // Resolve registry: explicit flag wins; otherwise auto-discover via
    // existing context (kubectl current-context's ACR is recorded by
    // `kars context`). For the CLI we read it from azd / config
    // by shelling out — but to keep this slice tight, we require
    // either --registry or KARS_REGISTRY.
    const registry =
      options.registry ||
      process.env.KARS_REGISTRY ||
      (await discoverRegistry());
    if (!registry) {
      throw new Error(
        `--registry not set and could not auto-discover. Pass --registry <acr.azurecr.io> or set KARS_REGISTRY.`,
      );
    }
    const repository = options.repository || `policy/egress-allowlist/${name}`;

    // Read live KarsSandbox state — generation + endpoints.
    const state = await readKarsSandboxState({
      kubectlPath: "kubectl",
      namespace: crNamespace,
      name,
    });
    if (state.endpoints.length === 0) {
      throw new Error(
        `KarsSandbox ${crNamespace}/${name} has no spec.networkPolicy.allowedEndpoints — refusing to sign empty allowlist.`,
      );
    }

    const canonical = buildCanonicalAllowlist({
      generation: state.generation,
      endpoints: state.endpoints,
    });

    const mode = autoDetectSignMode({
      signModeFlag: options.signMode,
      signKey: options.signKey,
      isTTY: Boolean(process.stdout.isTTY),
      env: process.env,
    });

    console.log(chalk.dim(`     Registry:   ${registry}/${repository}`));
    console.log(chalk.dim(`     Generation: ${state.generation}`));
    console.log(chalk.dim(`     Endpoints:  ${canonical.endpoints.length}`));
    for (const ep of canonical.endpoints) {
      const proto = ep.protocol ? `${ep.protocol}://` : "";
      console.log(chalk.dim(`                   • ${proto}${ep.host}:${ep.port}`));
    }
    console.log(chalk.dim(`     Sign mode:  ${mode}`));

    // Pre-flight: oras and cosign authenticate via the local Docker /
    // ORAS keychain; both require a prior `az acr login --name <acr>`
    // (or equivalent docker login). Without it, `oras push` returns a
    // 401 from the registry's OAuth token endpoint with a multi-line
    // error that's hard to interpret. Try auto-login when we detect az
    // is available, then surface a single-line actionable error if it
    // still fails. This is best-effort — if `az` isn't in PATH we just
    // proceed and let oras fail with the real error.
    await ensureAcrAuth(registry);

    const digest = await pushArtifact({
      orasPath,
      registry,
      repository,
      yaml: canonical.yaml,
      artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
    });
    console.log(chalk.green(`     ✅ Pushed   ${digest}`));

    try {
      await signArtifact({
        cosignPath,
        registry,
        repository,
        digest,
        mode,
        keyRef: options.signKey,
      });
    } catch (e: any) {
      // Fail-closed: do NOT patch the CR if signing failed.
      throw new Error(`cosign sign failed (CR not patched): ${e.message}`);
    }
    console.log(chalk.green(`     ✅ Signed   (mode=${mode})`));

    if (options.emitManifest) {
      // S12.g — GitOps mode. Skip kubectl patch; write a byte-stable
      // KarsSandbox manifest the operator commits to their GitOps
      // repo. The cluster never sees this command.
      const manifest = buildEmitManifestYaml({
        namespace: crNamespace,
        name,
        registry,
        repository,
        digest,
        artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
        signerIdentity: describeSignerIdentity({
          mode,
          keyRef: options.signKey,
          env: process.env,
        }),
      });
      try {
        writeEmitManifest({
          path: options.emitManifest,
          yaml: manifest,
          force: Boolean(options.force),
        });
      } catch (e: any) {
        throw new Error(e.message);
      }
      console.log(
        chalk.green(`     ✅ Wrote     ${options.emitManifest}`),
      );
      console.log();
      console.log(
        chalk.hex("#0078D4")(
          `  → Commit this file and apply via your GitOps controller.`,
        ),
      );
      console.log();
      return true;
    }

    await patchKarsSandbox({
      kubectlPath: "kubectl",
      namespace: crNamespace,
      name,
      registry,
      repository,
      digest,
      artifactType: EGRESS_ALLOWLIST_MEDIA_TYPE,
    });
    console.log(chalk.green(`     ✅ Patched  spec.networkPolicy.allowlistRef`));
    console.log(chalk.dim(`\n  The controller will verify the artifact and program NetworkPolicy egress on next reconcile (authoritative mode).\n`));
    return true;
  } catch (e: any) {
    console.log(chalk.red(`\n  Signing aborted: ${e.message}\n`));
    process.exitCode = 1;
    return false;
  }
}

/**
 * Best-effort ACR pre-auth so `oras push` doesn't 401. The ORAS keychain
 * reads ~/.docker/config.json (and the credential helpers it points at);
 * `az acr login` is the canonical way to populate it. If `az` is missing
 * we skip — the oras call may still succeed with cached creds, and if
 * not the error message will be the same as before this helper existed.
 */
async function ensureAcrAuth(registry: string): Promise<void> {
  // Strip any path component — we only want the registry FQDN.
  const fqdn = registry.split("/")[0];
  if (!fqdn.endsWith(".azurecr.io")) return;
  const acrName = fqdn.replace(/\.azurecr\.io$/, "");
  const { execa } = await import("execa");
  // Confirm `az` is on PATH; bail silently if not.
  try {
    await execa("az", ["--version"], { stdio: "pipe", timeout: 5_000 });
  } catch {
    return;
  }
  // Fast-path: probe if we already have a fresh token. `az acr login`
  // is idempotent and cheap, so we just always run it (it returns
  // 'Login Succeeded' in <2s when already authenticated).
  console.log(chalk.dim(`     ACR auth:   ensuring login to ${fqdn} (az acr login --name ${acrName})`));
  try {
    await execa("az", ["acr", "login", "--name", acrName], { stdio: "pipe", timeout: 30_000 });
    console.log(chalk.dim(`     ACR auth:   ✓ logged in`));
  } catch (e: any) {
    // Surface a one-liner, but don't fail the whole flow yet — let oras
    // try and report the actual 401 if the credential is genuinely bad.
    const tail = String(e?.stderr ?? e?.stdout ?? e?.message ?? "")
      .split("\n").map((s: string) => s.trim()).filter(Boolean).slice(-1)[0] ?? "unknown";
    console.log(chalk.yellow(`     ACR auth:   ⚠ az acr login failed (${tail.substring(0, 120)}). Continuing — oras may still have cached creds.`));
  }
}


async function discoverRegistry(): Promise<string | null> {
  // Best-effort lookup from the CLI's config file. Keeping this thin
  // — the explicit --registry flag is the documented path.
  try {
    const { loadContext } = await import("../config.js");
    const ctx = loadContext();
    const reg = (ctx as any)?.acrLoginServer || (ctx as any)?.registry || null;
    return typeof reg === "string" && reg.length > 0 ? reg : null;
  } catch {
    return null;
  }
}

/**
 * Find the namespace where the KarsSandbox CR lives. The pod-namespace
 * `kars-<name>` (used as a fallback) is where the *pod* runs, but
 * the controller creates the KarsSandbox CR in its own release namespace
 * (default `kars-system`). Earlier sign attempts were querying the
 * pod ns and failing with `karssandbox/<name> not found`.
 *
 * Strategy: try the operator's standard namespace first (cheap, fast),
 * then fall back to a cross-namespace lookup, and finally to the
 * pod-namespace if everything else fails (preserves legacy behavior
 * for unusual setups). Surfaces a clear error rather than letting
 * a downstream kubectl call fail with a confusing 'not found'.
 */
async function readRawAllowedEndpoints(
  crNamespace: string,
  name: string,
): Promise<RawEndpoint[]> {
  const { execa } = await import("execa");
  const { stdout } = await execa("kubectl", [
    "get", `karssandbox/${name}`, "-n", crNamespace, "-o", "json",
  ], { stdio: "pipe" });
  const obj = JSON.parse(stdout || "{}");
  const raw: unknown = obj?.spec?.networkPolicy?.allowedEndpoints;
  const out: RawEndpoint[] = [];
  if (Array.isArray(raw)) {
    for (const ep of raw) {
      const e = ep as { host?: unknown; port?: unknown };
      if (typeof e.host === "string" && e.host) {
        out.push(typeof e.port === "number" ? { host: e.host, port: e.port } : { host: e.host });
      }
    }
  }
  return out;
}

async function patchBaselineEndpoints(
  crNamespace: string,
  name: string,
  endpoints: RawEndpoint[],
): Promise<{ normalized: number }> {
  const { execa } = await import("execa");
  // The signed allowlist requires an explicit port on every entry (the
  // canonical builder rejects port-less), and `readKarsSandboxState` (used by
  // the sign flow) silently drops port-less entries. To keep the persisted
  // baseline sign-compatible AND avoid silently losing an existing host on the
  // next re-sign, normalize any port-less entry to :443 here.
  let normalized = 0;
  const withPorts = endpoints.map((e) => {
    if (e.port === undefined) {
      normalized += 1;
      return { host: e.host, port: 443 };
    }
    return e;
  });
  // JSON merge patch: a list value replaces the existing list wholesale, so we
  // pass the full desired union/remainder. Other networkPolicy fields
  // (egressMode, allowlistRef) are preserved by the merge.
  const patch = { spec: { networkPolicy: { allowedEndpoints: withPorts } } };
  await execa("kubectl", [
    "patch", `karssandbox/${name}`, "-n", crNamespace,
    "--type", "merge", "-p", JSON.stringify(patch),
  ], { stdio: "pipe" });
  return { normalized };
}

async function patchEgressMode(
  crNamespace: string,
  name: string,
  mode: "Strict" | "Learn",
): Promise<void> {
  const { execa } = await import("execa");
  const patch = { spec: { networkPolicy: { egressMode: mode } } };
  await execa("kubectl", [
    "patch", `karssandbox/${name}`, "-n", crNamespace,
    "--type", "merge", "-p", JSON.stringify(patch),
  ], { stdio: "pipe" });
}

async function discoverKarsSandboxNamespace(name: string, podNs: string): Promise<string> {
  const { execa } = await import("execa");
  // 1) Operator default — covers >99% of installs.
  try {
    await execa("kubectl", [
      "get", `karssandbox/${name}`, "-n", "kars-system", "-o", "name",
    ], { stdio: "pipe", timeout: 5_000 });
    return "kars-system";
  } catch {
    /* fall through */
  }
  // 2) Cross-namespace lookup — handles non-default operator releases.
  try {
    const { stdout } = await execa("kubectl", [
      "get", "karssandbox", "-A", "-o",
      `jsonpath={range .items[?(@.metadata.name=="${name}")]}{.metadata.namespace}{"\\n"}{end}`,
    ], { stdio: "pipe", timeout: 5_000 });
    const ns = stdout.trim().split("\n").map((s) => s.trim()).filter(Boolean)[0];
    if (ns) return ns;
  } catch {
    /* fall through */
  }
  // 3) Last-ditch: pod ns. Will likely fail downstream with a clear
  //    "not found" — and the operator can pass --namespace explicitly.
  throw new Error(
    `KarsSandbox '${name}' not found in 'kars-system' or any other namespace. ` +
    `Pass --namespace <ns> to specify the operator's release namespace.`,
  );
  // (intentionally unused; kept to silence linter about podNs param)
  void podNs;
}
