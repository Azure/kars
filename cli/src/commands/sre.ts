// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { execa } from "execa";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Resolve the kars repo root.
 *
 * Strategy mirrors `cli/src/commands/up.ts`: first try the
 * three-levels-up-from-the-installed-CLI-file path (works for
 * `npm link` installs), then fall back to walking up from CWD
 * looking for `deploy/helm`.
 */
function resolveRepoRoot(): string {
  // Strategy 1: from the file's own location (works for npm link
  // since the link points back into the repo's cli/dist/ tree)
  try {
    const thisFile = fileURLToPath(import.meta.url);
    const cliDir = path.dirname(path.dirname(thisFile)); // .../cli/dist
    const candidate = path.resolve(cliDir, "..", "..");  // .../<repo>
    if (fs.existsSync(path.join(candidate, "deploy", "helm", "kars"))) {
      return candidate;
    }
  } catch {
    // import.meta.url may not be a file URL in some test contexts
  }
  // Strategy 2: walk up from CWD looking for deploy/helm
  let cur = process.cwd();
  for (let i = 0; i < 8; i++) {
    if (fs.existsSync(path.join(cur, "deploy", "helm", "kars"))) return cur;
    const parent = path.dirname(cur);
    if (parent === cur) break;
    cur = parent;
  }
  throw new Error(
    "Could not resolve the kars repo root (looked for deploy/helm/kars). " +
    "Run `kars sre install` from inside an kars checkout, or set the working " +
    "directory to the repo root first.",
  );
}

/**
 * `kars sre` — manage the built-in kars-sre agent.
 *
 * Subcommands:
 *   install      — enable the chart's sre.yaml template (helm upgrade --set sre.enabled=true)
 *   uninstall    — disable it (helm upgrade --set sre.enabled=false)
 *   status       — show the sre KarsSandbox CR's state (kubectl get karssandbox sre)
 *   talk         — alias for `kars connect sre` (open the WebUI)
 *
 * Design: docs/blueprints/07-kars-sre-proposal.md
 */
export function sreCommand(): Command {
  const cmd = new Command("sre");
  cmd.description("Manage the built-in kars-sre agent (Kubernetes SRE on the cluster)");

  cmd
    .command("install")
    .description("Enable the kars-sre agent on the current cluster")
    .option(
      "--release <name>",
      "Helm release name to patch (defaults to 'kars')",
      "kars",
    )
    .option(
      "--namespace <ns>",
      "Helm release namespace (defaults to 'kars-system')",
      "kars-system",
    )
    .option(
      "--context <name>",
      "kubectl context to use (defaults to current-context)",
    )
    .option(
      "--model <name>",
      "Azure OpenAI deployment / model name for the SRE agent (defaults to gpt-5.4)",
    )
    .option(
      "--no-wait",
      "Don't wait for the sre sandbox to reach Running (default: wait)",
    )
    .action(async (options: {
      release: string;
      namespace: string;
      context?: string;
      model?: string;
      wait: boolean;
    }) => {
      let chartPath: string;
      try {
        chartPath = path.join(resolveRepoRoot(), "deploy", "helm", "kars");
      } catch (err: any) {
        console.error(chalk.red(`✗ ${err.message}`));
        process.exit(1);
      }

      // Detect deployment shape:
      //   A. operator deployed via `helm install` (release tracked) →
      //      use `helm upgrade --reuse-values`
      //   B. operator deployed via `kars dev --target local-k8s`
      //      (which renders `helm template | kubectl apply` and so
      //      never creates a helm release record) → use `helm template
      //      | kubectl apply --server-side --force-conflicts` with
      //      `sre.enabled=true` baked in. The chart is already in
      //      the cluster; this just adds the SRE bits idempotently.
      //   C. no chart at all → `helm install` with --take-ownership +
      //      a fallback workload-identity client-id (local dev).
      let mode: "upgrade" | "template" | "install" = "install";
      const listArgs = ["list", "-n", options.namespace, "-q"];
      if (options.context) listArgs.push("--kube-context", options.context);
      try {
        const { stdout } = await execa("helm", listArgs, { stdio: "pipe" });
        if (
          stdout
            .split(/\r?\n/)
            .map(s => s.trim())
            .includes(options.release)
        ) {
          mode = "upgrade";
        }
      } catch {
        // helm list errored — treat as "not installed"
      }
      if (mode === "install") {
        // Check whether the controller already runs in the namespace.
        // Presence implies `kars dev` deployed it via `helm template
        // | kubectl apply` — adopting via plain `helm install` would
        // fail on every pre-existing resource. Take the template path.
        try {
          await execa(
            "kubectl",
            [
              ...(options.context ? ["--context", options.context] : []),
              "-n", options.namespace,
              "get", "deploy/kars-controller",
            ],
            { stdio: "ignore" },
          );
          mode = "template";
        } catch {
          // Controller missing → fresh cluster → safe to helm install.
        }
      }

      const helmArgs =
        mode === "upgrade"
          ? [
              "upgrade",
              options.release,
              chartPath,
              "--namespace", options.namespace,
              // --reset-then-reuse-values: re-load defaults from values.yaml
              // THEN overlay the previously-set --set values. Critical for
              // operators upgrading from older chart versions whose stored
              // release values predate fields like runtimes.hermes — a plain
              // --reuse-values would carry the gap forward and fail templating.
              "--reset-then-reuse-values",
              // --force-conflicts: helm 4 uses server-side apply by default,
              // which conflicts with field managers from prior `kubectl set
              // image` / `kars push --apply` runs that touched the same
              // fields. This flag tells SSA to take ownership on conflict,
              // matching the operator's intent (helm-managed chart is the
              // source of truth).
              "--force-conflicts",
              "--set", "sre.enabled=true",
            ]
          : mode === "template"
          ? [
              "template",
              options.release,
              chartPath,
              "--namespace", options.namespace,
              "--include-crds",
              "--set", "sre.enabled=true",
              // Placeholder client-id — same default kars dev uses.
              // Local-k8s clusters never federate to Entra so this
              // value is purely a template-completeness shim.
              "--set", "azure.workloadIdentity.clientId=dummy",
            ]
          : [
              "install",
              options.release,
              chartPath,
              "--namespace", options.namespace,
              "--create-namespace",
              "--force-conflicts",
              // --take-ownership: adopt resources that already exist in the
              // cluster but don't carry helm metadata (the kars-system
              // namespace, default-deny NetworkPolicy, etc. created
              // out-of-band by a prior `kars dev` or partial helm
              // install). Without this, install dies on the first such
              // resource with a "cannot be imported" error. Requires
              // helm >= 3.17 (`kars dev` pins helm 4 — safe).
              "--take-ownership",
              "--set", "sre.enabled=true",
              // Brand-new chart install on a fresh cluster has no prior
              // azure.workloadIdentity.clientId — use a dummy fallback for
              // local-k8s dev. Real AKS installs come through `kars up`
              // which sets this properly.
              "--set", "azure.workloadIdentity.clientId=dummy",
            ];
      if (options.model) helmArgs.push("--set", `sre.model=${options.model}`);
      if (options.context) helmArgs.push("--kube-context", options.context);

      const verbHuman =
        mode === "upgrade" ? "upgrade"
        : mode === "template" ? "template | kubectl apply"
        : "install";
      console.log(chalk.cyan(`▸ enabling kars-sre via helm ${verbHuman}…`));
      console.log(chalk.gray(`  helm ${helmArgs.join(" ")}`));
      try {
        if (mode === "template") {
          // Render the chart, then apply via kubectl SSA — same flow
          // kars dev --target local-k8s uses. We pipe stdout → kubectl
          // apply to avoid a tempfile and to inherit kubectl's own
          // diff/error formatting.
          const { stdout } = await execa("helm", helmArgs, { stdio: "pipe" });
          const kctxArgs = options.context ? ["--context", options.context] : [];
          await execa(
            "kubectl",
            [
              ...kctxArgs,
              "apply",
              "-f", "-",
              "--server-side",
              "--force-conflicts",
            ],
            {
              input: stdout,
              stdio: ["pipe", "inherit", "inherit"],
            },
          );
        } else {
          await execa("helm", helmArgs, { stdio: "inherit" });
        }
      } catch {
        console.error(chalk.red(`✗ helm ${verbHuman} failed`));
        process.exit(1);
      }
      console.log(chalk.green("✓ chart patched"));

      if (options.wait) {
        const kctxArgs = options.context ? ["--context", options.context] : [];
        console.log(chalk.cyan("▸ waiting for kars-sre namespace to appear…"));
        for (let i = 0; i < 60; i++) {
          try {
            await execa("kubectl", [...kctxArgs, "get", "ns", "kars-sre"], { stdio: "ignore" });
            console.log(chalk.green("✓ kars-sre namespace exists"));
            break;
          } catch {
            await new Promise((r) => setTimeout(r, 1000));
          }
        }
        console.log(chalk.cyan("▸ waiting for sre sandbox to reach Available (up to 180s)…"));
        try {
          await execa(
            "kubectl",
            [
              ...kctxArgs,
              "-n", "kars-sre",
              "wait",
              "--for=condition=Available",
              "deploy/sre",
              "--timeout=180s",
            ],
            { stdio: "inherit" },
          );
          console.log(chalk.green("✓ kars-sre is ready"));
          console.log("");
          console.log(`  ${chalk.bold("Next:")}  ${chalk.cyan("kars sre talk")}    (open the WebUI)`);
          console.log(`         ${chalk.cyan("kars sre status")}  (CR + pod state)`);
        } catch {
          console.warn(chalk.yellow("⚠ sre sandbox did not become Available within 180s"));
          console.warn(chalk.yellow("  Run `kars sre status` to inspect."));
          process.exit(1);
        }
      }
    });

  cmd
    .command("uninstall")
    .description("Disable the kars-sre agent (the namespace + RBAC are torn down by the controller)")
    .option("--release <name>", "Helm release name", "kars")
    .option("--namespace <ns>", "Helm release namespace", "kars-system")
    .option("--context <name>", "kubectl context to use")
    .action(async (options: { release: string; namespace: string; context?: string }) => {
      let chartPath: string;
      try {
        chartPath = path.join(resolveRepoRoot(), "deploy", "helm", "kars");
      } catch (err: any) {
        console.error(chalk.red(`✗ ${err.message}`));
        process.exit(1);
      }

      const helmArgs = [
        "upgrade",
        options.release,
        chartPath,
        "--namespace", options.namespace,
        "--reset-then-reuse-values",
        "--force-conflicts",
        "--set", "sre.enabled=false",
      ];
      if (options.context) helmArgs.push("--kube-context", options.context);

      console.log(chalk.cyan("▸ disabling kars-sre via helm upgrade --reuse-values…"));
      try {
        await execa("helm", helmArgs, { stdio: "inherit" });
      } catch {
        console.error(chalk.red("✗ helm upgrade failed"));
        process.exit(1);
      }
      console.log(chalk.green("✓ kars-sre disabled; controller will garbage-collect the sandbox + namespace"));
    });

  cmd
    .command("status")
    .description("Show the sre KarsSandbox CR + pod state")
    .option("--context <name>", "kubectl context to use")
    .action(async (options: { context?: string }) => {
      const kctxArgs = options.context ? ["--context", options.context] : [];
      console.log(chalk.bold.cyan("── KarsSandbox sre (kars-system) ──"));
      try {
        await execa("kubectl", [...kctxArgs, "-n", "kars-system", "get", "karssandbox", "sre"], { stdio: "inherit" });
      } catch {
        console.error(chalk.yellow("⚠ KarsSandbox sre not found — run `kars sre install` first."));
        process.exit(1);
      }
      console.log("");
      console.log(chalk.bold.cyan("── pods (kars-sre namespace) ──"));
      try {
        await execa("kubectl", [...kctxArgs, "-n", "kars-sre", "get", "pod"], { stdio: "inherit" });
      } catch {
        console.warn(chalk.yellow("⚠ kars-sre namespace not yet provisioned"));
      }
    });

  cmd
    .command("talk")
    .description("Open the kars-sre WebUI (alias for `kars connect sre`)")
    .option("--context <name>", "kubectl context to use")
    .option("--port <port>", "Local port for WebUI port-forward", "18790")
    .action(async (options: { context?: string; port: string }) => {
      const args = ["connect", "sre", "--web", "--port", options.port];
      if (options.context) args.push("--context", options.context);
      console.log(chalk.cyan(`▸ kars connect sre (WebUI on http://localhost:${options.port})…`));
      try {
        await execa("kars", args, { stdio: "inherit" });
      } catch {
        console.error(chalk.red("✗ failed to connect — try `kars sre status` to verify the sandbox is Running"));
        process.exit(1);
      }
    });

  // ──────────────────────────────────────────────────────────────────
  // Slice 3 — Typed apply-fix approval surface (KarsSREAction)
  //
  // The SRE agent diagnoses, then EMITS a KarsSREAction CR in
  // `kars-sre`. Phase=Proposed, approval.state=Pending. The operator
  // uses these subcommands to approve / reject / list. On approve, the
  // kars-controller's kars_sre_action reconciler mints a one-shot
  // ClusterRoleBinding, executes the typed action, and tears the
  // binding down. The whole flow is one CR per incident.
  // ──────────────────────────────────────────────────────────────────
  cmd
    .command("approve <action-id>")
    .description("Approve a pending KarsSREAction proposal — authorises the controller to execute")
    .option("--context <name>", "kubectl context to use")
    .option("--note <text>", "Optional human-readable note attached to the decision (surfaces in audit)")
    .action(async (actionId: string, options: { context?: string; note?: string }) => {
      const kctxArgs = options.context ? ["--context", options.context] : [];
      const patch: { spec: { approval: { state: string; note?: string } } } = {
        spec: { approval: { state: "Approved" } },
      };
      if (options.note) patch.spec.approval.note = options.note;
      console.log(chalk.cyan(`▸ approving KarsSREAction ${actionId}…`));
      try {
        await execa(
          "kubectl",
          [
            ...kctxArgs,
            "-n",
            "kars-sre",
            "patch",
            "karssreaction",
            actionId,
            "--type=merge",
            "-p",
            JSON.stringify(patch),
          ],
          { stdio: "inherit" },
        );
        console.log(chalk.green(`✓ approved — controller will execute on next reconcile`));
        console.log(chalk.dim(`  watch:  kubectl -n kars-sre get karssreaction ${actionId} -w`));
      } catch {
        console.error(chalk.red(`✗ approve failed — does ${actionId} exist in kars-sre?`));
        process.exit(1);
      }
    });

  cmd
    .command("reject <action-id>")
    .description("Reject a pending KarsSREAction proposal — controller will NOT execute")
    .option("--context <name>", "kubectl context to use")
    .option("--reason <text>", "Optional reason for the rejection (surfaces in audit)")
    .action(async (actionId: string, options: { context?: string; reason?: string }) => {
      const kctxArgs = options.context ? ["--context", options.context] : [];
      const patch: { spec: { approval: { state: string; note?: string } } } = {
        spec: { approval: { state: "Rejected" } },
      };
      if (options.reason) patch.spec.approval.note = options.reason;
      console.log(chalk.cyan(`▸ rejecting KarsSREAction ${actionId}…`));
      try {
        await execa(
          "kubectl",
          [
            ...kctxArgs,
            "-n",
            "kars-sre",
            "patch",
            "karssreaction",
            actionId,
            "--type=merge",
            "-p",
            JSON.stringify(patch),
          ],
          { stdio: "inherit" },
        );
        console.log(chalk.green(`✓ rejected`));
      } catch {
        console.error(chalk.red(`✗ reject failed — does ${actionId} exist in kars-sre?`));
        process.exit(1);
      }
    });

  cmd
    .command("actions")
    .description("List recent KarsSREAction proposals (alias: `kubectl get karssreactions -n kars-sre`)")
    .option("--context <name>", "kubectl context to use")
    .option("--all-namespaces", "List from every namespace (operator may have created elsewhere)")
    .action(async (options: { context?: string; allNamespaces?: boolean }) => {
      const kctxArgs = options.context ? ["--context", options.context] : [];
      const scopeArgs = options.allNamespaces ? ["-A"] : ["-n", "kars-sre"];
      try {
        await execa(
          "kubectl",
          [...kctxArgs, ...scopeArgs, "get", "karssreactions"],
          { stdio: "inherit" },
        );
      } catch {
        console.error(chalk.yellow("⚠ no KarsSREActions yet — agent emits these on `sre_propose_fix`"));
      }
    });

  cmd
    .command("show <action-id>")
    .description("Show the full details of a KarsSREAction proposal — diagnosis, rationale, action target, approval state, status conditions. Use this before `kars sre approve` to review what you're authorising.")
    .option("--context <name>", "kubectl context to use")
    .option("--yaml", "Print raw YAML instead of the pretty summary")
    .action(async (actionId: string, options: { context?: string; yaml?: boolean }) => {
      const kctxArgs = options.context ? ["--context", options.context] : [];
      if (options.yaml) {
        try {
          await execa(
            "kubectl",
            [...kctxArgs, "-n", "kars-sre", "get", "karssreaction", actionId, "-o", "yaml"],
            { stdio: "inherit" },
          );
        } catch {
          console.error(chalk.red(`✗ ${actionId} not found in kars-sre`));
          process.exit(1);
        }
        return;
      }
      // Pretty-print: fetch JSON and format key fields.
      let cr: {
        metadata?: { name?: string; namespace?: string; creationTimestamp?: string };
        spec?: {
          action?: { type?: string; params?: Record<string, unknown> };
          approval?: { state?: string; note?: string };
          diagnosis?: string;
          rationale?: string;
          ttlMinutes?: number;
        };
        status?: {
          phase?: string;
          appliedAt?: string;
          writerCrbName?: string;
          conditions?: Array<{ type: string; status: string; reason?: string; message?: string }>;
        };
      };
      try {
        const { stdout } = await execa(
          "kubectl",
          [...kctxArgs, "-n", "kars-sre", "get", "karssreaction", actionId, "-o", "json"],
          { stdio: "pipe" },
        );
        cr = JSON.parse(stdout);
      } catch {
        console.error(chalk.red(`✗ ${actionId} not found in kars-sre`));
        process.exit(1);
        return;
      }
      const spec = cr.spec ?? {};
      const status = cr.status ?? {};
      const action = spec.action ?? {};
      const approval = spec.approval ?? {};
      const phase = status.phase ?? chalk.dim("(not yet reconciled)");
      const approvalState = approval.state ?? chalk.dim("(unset)");
      const phaseColour =
        status.phase === "Recovered"
          ? chalk.green
          : status.phase === "Applied"
          ? chalk.cyan
          : status.phase === "Failed" || status.phase === "Rejected" || status.phase === "Expired"
          ? chalk.red
          : chalk.yellow;
      const approvalColour =
        approval.state === "Approved"
          ? chalk.green
          : approval.state === "Rejected"
          ? chalk.red
          : chalk.yellow;

      console.log("");
      console.log(chalk.bold.cyan(`── KarsSREAction ${actionId} ──`));
      console.log(`  ${chalk.bold("Namespace:")}     ${cr.metadata?.namespace ?? "?"}`);
      console.log(`  ${chalk.bold("Created:")}       ${cr.metadata?.creationTimestamp ?? "?"}`);
      console.log(`  ${chalk.bold("Phase:")}         ${phaseColour(phase)}`);
      console.log(`  ${chalk.bold("Approval:")}      ${approvalColour(approvalState)}`);
      if (approval.note) {
        console.log(`  ${chalk.bold("Approver note:")} ${approval.note}`);
      }
      if (spec.ttlMinutes) {
        console.log(`  ${chalk.bold("TTL minutes:")}   ${spec.ttlMinutes}`);
      }
      console.log("");
      console.log(chalk.bold.cyan("── Proposed action ──"));
      console.log(`  ${chalk.bold("Type:")}          ${chalk.magenta(action.type ?? "?")}`);
      if (action.params) {
        for (const [k, v] of Object.entries(action.params)) {
          console.log(`  ${chalk.bold(k.padEnd(13) + ":")} ${typeof v === "string" ? v : JSON.stringify(v)}`);
        }
      }
      if (spec.diagnosis) {
        console.log("");
        console.log(chalk.bold.cyan("── Diagnosis ──"));
        console.log(`  ${spec.diagnosis}`);
      }
      if (spec.rationale) {
        console.log("");
        console.log(chalk.bold.cyan("── Rationale ──"));
        // Wrap at ~88 cols for readable terminal output
        const wrapped = spec.rationale.match(/.{1,88}(\s|$)|\S+/g) ?? [spec.rationale];
        for (const line of wrapped) console.log(`  ${line.trim()}`);
      }
      if (status.appliedAt || status.writerCrbName) {
        console.log("");
        console.log(chalk.bold.cyan("── Execution ──"));
        if (status.appliedAt) console.log(`  ${chalk.bold("Applied at:")}   ${status.appliedAt}`);
        if (status.writerCrbName)
          console.log(`  ${chalk.bold("Writer CRB:")}   ${status.writerCrbName}`);
      }
      if (status.conditions && status.conditions.length) {
        console.log("");
        console.log(chalk.bold.cyan("── Conditions ──"));
        for (const c of status.conditions) {
          const sym = c.status === "True" ? chalk.green("✓") : chalk.yellow("·");
          const reason = c.reason ? chalk.dim(`(${c.reason})`) : "";
          console.log(`  ${sym} ${chalk.bold(c.type.padEnd(10))} ${c.status}  ${reason}`);
          if (c.message) console.log(`     ${chalk.dim(c.message)}`);
        }
      }
      console.log("");
      if (approval.state !== "Approved" && approval.state !== "Rejected") {
        console.log(chalk.dim(`  approve:  kars sre approve ${actionId}`));
        console.log(chalk.dim(`  reject:   kars sre reject ${actionId} --reason "..."`));
      }
      console.log("");
    });

  return cmd;
}
