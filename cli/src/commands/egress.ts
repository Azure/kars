// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { getAdminToken, withAdminAuth } from "../router-admin.js";
import {
  EGRESS_ALLOWLIST_MEDIA_TYPE,
  autoDetectSignMode,
  buildCanonicalAllowlist,
  buildEmitManifestYaml,
  describeSignerIdentity,
  ensureSigningTools,
  patchClawSandbox,
  pushArtifact,
  readClawSandboxState,
  signArtifact,
  writeEmitManifest,
} from "./egress/sign.js";

export function egressCommand(): Command {
  const cmd = new Command("egress");

  cmd
    .description("Manage network egress: allowlist, approvals, and learn mode")
    .argument("[name]", "Sandbox name (default: demo-agent)", "demo-agent")
    .option("--namespace <ns>", "Kubernetes namespace")
    .option("--learn", "Enable learn mode (log all accessed domains)")
    .option("--no-learn", "Disable learn mode")
    .option("--learned", "Show domains discovered during learn mode")
    .option("--pending", "Show domains pending operator approval")
    .option("--approve <domain>", "Approve a domain for egress")
    .option("--deny <domain>", "Deny and remove a pending domain request")
    .option("--allowlist", "Show currently approved domains")
    .option("--enforce", "Graduate: promote all learned domains to allowlist, switch to enforcement mode")
    .option("--status", "Show blocklist and learn mode status")
    .option("--sign", "Build canonical allowlist artifact, push to OCI registry, sign with cosign, patch allowlistRef. **Default-on** when combined with --enforce or --approve. Pass --no-sign to opt out.")
    .option("--no-sign", "Skip signing. The controller will refuse to use the artifact in authoritative mode (SignerPolicyMissing). Use only for local dev.")
    .option("--sign-mode <mode>", "Cosign mode: keyless | identity-token | keyed (default: auto-detect)")
    .option("--sign-key <ref>", "Cosign key reference (path or KMS URI like azurekms://...) — required for --sign-mode keyed")
    .option("--registry <fqdn>", "Override target ACR for the artifact push (default: auto-discover)")
    .option("--repository <repo>", "Repository path within the registry (default: policy/egress-allowlist/<sandbox>)")
    .option("--emit-manifest <path>", "GitOps mode: write the ClawSandbox patch to <path> instead of running 'kubectl patch'. Requires signing (default-on). Refuses to overwrite without --force.")
    .option("--force", "With --emit-manifest, overwrite an existing file.")
    .action(async (name: string, options) => {
      const { execa } = await import("execa");

      // S12.g — sign-by-default. When the operator runs --enforce or
      // --approve, signing happens automatically unless --no-sign is
      // passed. options.sign is:
      //   - undefined → not specified → default to true in signing context
      //   - true      → user passed --sign explicitly
      //   - false     → user passed --no-sign
      const inSigningContext = Boolean(options.enforce || options.approve);
      const signRequested =
        options.sign === false ? false : (options.sign === true || inSigningContext);

      // --emit-manifest implies a signing context; require --enforce or --approve.
      if (options.emitManifest && !inSigningContext) {
        console.log(
          chalk.red(
            `\n  --emit-manifest requires --enforce or --approve (the artifact is built from the live allowlist).\n`,
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
        console.log(chalk.red(`\n  --sign requires --enforce or --approve.\n`));
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

      const containerName = `azureclaw-${name}`;
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
      const adminToken = mode === "k8s" ? await getAdminToken(ns) : "";

      // Helper: call router API — Docker exec or kubectl exec
      async function routerGet(path: string): Promise<any> {
        let curlArgs = mode === "docker"
          ? ["exec", containerName, "curl", "-s", `http://127.0.0.1:8443${path}`]
          : ["exec", "-n", ns, pod, "-c", "inference-router", "--",
             ...withAdminAuth(["curl", "-s", `http://127.0.0.1:8443${path}`], adminToken)];
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
             ...withAdminAuth(["curl", "-s", "-X", "POST",
             "-H", "Content-Type: application/json",
             "-d", JSON.stringify(body),
             `http://127.0.0.1:8443${path}`], adminToken)];
        const bin = mode === "docker" ? "docker" : "kubectl";
        const { stdout } = await execa(bin, curlArgs, { stdio: "pipe" });
        return JSON.parse(stdout);
      }

      // Approve a domain
      if (options.approve) {
        try {
          const result = await routerPost("/egress/approve", { domain: options.approve });
          console.log(chalk.green(`\n  ✅ Approved: ${result.domain}`));
          console.log(chalk.dim(`     Domain added to egress allowlist. The agent can now reach it.\n`));
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to approve: ${e.message}\n`));
          return;
        }
        if (signRequested) {
          await runSignFlow(name, ns, options);
        }
        return;
      }

      // Deny a domain
      if (options.deny) {
        try {
          const result = await routerPost("/egress/deny", { domain: options.deny });
          console.log(chalk.yellow(`\n  ❌ Denied: ${result.domain}\n`));
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to deny: ${e.message}\n`));
        }
        return;
      }

      // Enforce: graduate from learn mode to enforcement
      if (options.enforce) {
        try {
          const result = await routerPost("/egress/enforce", {});
          if (result.status === "already_enforcing") {
            console.log(chalk.yellow(`\n  Already in enforcement mode.`));
            console.log(chalk.dim(`  Allowlist: ${result.allowlist_count} domain(s)\n`));
          } else {
            console.log(chalk.green(`\n  🔒 Enforcement mode activated for '${name}'`));
            console.log(chalk.dim(`     ${result.promoted} learned domain(s) promoted to allowlist`));
            console.log(chalk.dim(`     ${result.allowlist_count} total domain(s) in allowlist\n`));
            if (result.allowlist && result.allowlist.length > 0) {
              for (const domain of result.allowlist) {
                console.log(`    ${chalk.green("✓")} ${domain}`);
              }
              console.log();
            }
            console.log(chalk.dim(`  Learn mode is now OFF. Only allowlisted domains will pass.`));
            console.log(chalk.dim(`  New domains will go to pending approval.\n`));
            console.log(chalk.dim(`  Commands:`));
            console.log(chalk.dim(`    azureclaw egress ${name} --pending         Show pending requests`));
            console.log(chalk.dim(`    azureclaw egress ${name} --approve <domain> Approve a new domain`));
            console.log(chalk.dim(`    azureclaw egress ${name} --learn           Re-enable learn mode\n`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to enforce: ${e.message}\n`));
          return;
        }
        if (signRequested) {
          await runSignFlow(name, ns, options);
        }
        return;
      }

      // Show pending approvals
      if (options.pending) {
        try {
          const data = await routerGet("/egress/pending");
          console.log(chalk.hex("#0078D4")(`\n  Pending Egress Approvals for '${name}'`));
          if (data.pending && data.pending.length > 0) {
            console.log();
            for (const p of data.pending) {
              console.log(`    ${chalk.yellow("⏳")} ${chalk.white(p.domain)}`);
              console.log(chalk.dim(`       URL: ${p.url}`));
              console.log(chalk.dim(`       Time: ${p.timestamp}`));
              console.log(chalk.dim(`       Approve: azureclaw egress ${name} --approve ${p.domain}`));
              console.log();
            }
            console.log(chalk.dim(`  ${data.count} domain(s) pending approval.\n`));
          } else {
            console.log(chalk.dim(`\n    No pending requests.\n`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to query pending: ${e.message}\n`));
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

      // Enable learn mode
      if (options.learn === true) {
        try {
          await routerPost("/egress/learn", { enabled: true });
          console.log(chalk.green(`\n  ✅ Learn mode enabled for '${name}'.`));
          console.log(chalk.dim(`     All accessed domains will be logged (blocklist still enforced).`));
          console.log(chalk.dim(`     Run ${chalk.white(`azureclaw egress ${name} --learned`)} to see discovered domains.\n`));
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to enable learn mode: ${e.message}\n`));
        }
        return;
      }

      // Disable learn mode
      if (options.learn === false && process.argv.includes("--no-learn")) {
        try {
          await routerPost("/egress/learn", { enabled: false });
          console.log(chalk.yellow(`\n  Learn mode disabled for '${name}'.\n`));
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
        const [blStatus, allowlist, pending, learned] = await Promise.all([
          routerGet("/blocklist/status"),
          routerGet("/egress/allowlist"),
          routerGet("/egress/pending"),
          routerGet("/egress/learned").catch(() => ({ count: 0, domains: [] })),
        ]);
        console.log(chalk.hex("#0078D4")(`\n  Egress Security — '${name}'`));
        console.log(`    Blocklist:      ${blStatus.enabled ? chalk.green("enabled") : chalk.red("disabled")} (${blStatus.domain_count.toLocaleString()} domains)`);
        console.log(`    Learn mode:     ${blStatus.learn_mode ? chalk.green("ON") : chalk.dim("off")}`);
        console.log(`    Allowlist:      ${chalk.white(allowlist.count)} domain(s) approved`);
        console.log(`    Pending:        ${pending.count > 0 ? chalk.yellow(pending.count + " awaiting approval") : chalk.dim("none")}`);
        if (learned.count > 0) {
          console.log(`    Learned:        ${chalk.cyan(learned.count)} domain(s) discovered`);
        }
        console.log();
        if (pending.count > 0) {
          for (const p of pending.pending) {
            console.log(`    ${chalk.yellow("⏳")} ${p.domain}`);
          }
          console.log();
        }
        if (learned.count > 0 && blStatus.learn_mode) {
          console.log(chalk.dim(`  Discovered domains (learn mode):`));
          for (const d of learned.domains) {
            console.log(`    ${chalk.cyan("◉")} ${d}`);
          }
          console.log();
          console.log(chalk.hex("#0078D4")(`  → Ready to enforce? Run: ${chalk.white(`azureclaw egress ${name} --enforce`)}`));
          console.log(chalk.dim(`    This promotes ${learned.count} learned domain(s) to allowlist and activates enforcement.\n`));
        }
        console.log(chalk.dim(`  Commands:`));
        console.log(chalk.dim(`    azureclaw egress ${name} --enforce                Promote learned → allowlist, enforce`));
        console.log(chalk.dim(`    azureclaw egress ${name} --pending               Show pending requests`));
        console.log(chalk.dim(`    azureclaw egress ${name} --approve <domain>      Approve a domain`));
        console.log(chalk.dim(`    azureclaw egress ${name} --deny <domain>         Deny a domain`));
        console.log(chalk.dim(`    azureclaw egress ${name} --allowlist             Show approved domains`));
        console.log(chalk.dim(`    azureclaw egress ${name} --learned               Show discovered domains`));
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
): Promise<void> {
  const headerSlice = options.emitManifest ? "GitOps mode" : "sign-by-default";
  console.log(chalk.hex("#0078D4")(`\n  Signing egress allowlist artifact for '${name}' (${headerSlice})`));
  try {
    const { orasPath, cosignPath } = await ensureSigningTools();

    // The pod-namespace `azureclaw-<name>` is where the sandbox's pod, NetworkPolicy,
    // and per-sandbox secrets live — but the *ClawSandbox CR* itself is created
    // by the operator in the operator's release namespace (default
    // `azureclaw-system`). Read/patch always need the CR's namespace, NOT the
    // pod ns. Discover it once via cross-ns lookup.
    const crNamespace = await discoverClawSandboxNamespace(name, ns);

    // Resolve registry: explicit flag wins; otherwise auto-discover via
    // existing context (kubectl current-context's ACR is recorded by
    // `azureclaw context`). For the CLI we read it from azd / config
    // by shelling out — but to keep this slice tight, we require
    // either --registry or AZURECLAW_REGISTRY.
    const registry =
      options.registry ||
      process.env.AZURECLAW_REGISTRY ||
      (await discoverRegistry());
    if (!registry) {
      throw new Error(
        `--registry not set and could not auto-discover. Pass --registry <acr.azurecr.io> or set AZURECLAW_REGISTRY.`,
      );
    }
    const repository = options.repository || `policy/egress-allowlist/${name}`;

    // Read live ClawSandbox state — generation + endpoints.
    const state = await readClawSandboxState({
      kubectlPath: "kubectl",
      namespace: crNamespace,
      name,
    });
    if (state.endpoints.length === 0) {
      throw new Error(
        `ClawSandbox ${crNamespace}/${name} has no spec.networkPolicy.allowedEndpoints — refusing to sign empty allowlist.`,
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
      // ClawSandbox manifest the operator commits to their GitOps
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
      return;
    }

    await patchClawSandbox({
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
  } catch (e: any) {
    console.log(chalk.red(`\n  Signing aborted: ${e.message}\n`));
    process.exitCode = 1;
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
 * Find the namespace where the ClawSandbox CR lives. The pod-namespace
 * `azureclaw-<name>` (used as a fallback) is where the *pod* runs, but
 * the controller creates the ClawSandbox CR in its own release namespace
 * (default `azureclaw-system`). Earlier sign attempts were querying the
 * pod ns and failing with `clawsandbox/<name> not found`.
 *
 * Strategy: try the operator's standard namespace first (cheap, fast),
 * then fall back to a cross-namespace lookup, and finally to the
 * pod-namespace if everything else fails (preserves legacy behavior
 * for unusual setups). Surfaces a clear error rather than letting
 * a downstream kubectl call fail with a confusing 'not found'.
 */
async function discoverClawSandboxNamespace(name: string, podNs: string): Promise<string> {
  const { execa } = await import("execa");
  // 1) Operator default — covers >99% of installs.
  try {
    await execa("kubectl", [
      "get", `clawsandbox/${name}`, "-n", "azureclaw-system", "-o", "name",
    ], { stdio: "pipe", timeout: 5_000 });
    return "azureclaw-system";
  } catch {
    /* fall through */
  }
  // 2) Cross-namespace lookup — handles non-default operator releases.
  try {
    const { stdout } = await execa("kubectl", [
      "get", "clawsandbox", "-A", "-o",
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
    `ClawSandbox '${name}' not found in 'azureclaw-system' or any other namespace. ` +
    `Pass --namespace <ns> to specify the operator's release namespace.`,
  );
  // (intentionally unused; kept to silence linter about podNs param)
  void podNs;
}
