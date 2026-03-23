import { Command } from "commander";
import chalk from "chalk";

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
    .option("--status", "Show blocklist and learn mode status")
    .action(async (name: string, options) => {
      const { execa } = await import("execa");

      const ns = options.namespace || `azureclaw-${name}`;

      // Find the pod
      let pod: string;
      try {
        const { stdout } = await execa("kubectl", [
          "get", "pods", "-n", ns,
          "-o", `jsonpath={.items[?(@.status.phase=="Running")].metadata.name}`,
        ], { stdio: "pipe" });
        pod = stdout.trim().split(/\s+/)[0];
        if (!pod) throw new Error("no pod");
      } catch {
        console.log(chalk.red(`\n  No running pod found for '${name}' in namespace '${ns}'.\n`));
        return;
      }

      // Helper: call router API inside the pod
      async function routerGet(path: string): Promise<any> {
        const { stdout } = await execa("kubectl", [
          "exec", "-n", ns, pod, "-c", "inference-router", "--",
          "curl", "-s", `http://127.0.0.1:8443${path}`,
        ], { stdio: "pipe" });
        return JSON.parse(stdout);
      }

      async function routerPost(path: string, body: object): Promise<any> {
        const { stdout } = await execa("kubectl", [
          "exec", "-n", ns, pod, "-c", "inference-router", "--",
          "curl", "-s", "-X", "POST",
          "-H", "Content-Type: application/json",
          "-d", JSON.stringify(body),
          `http://127.0.0.1:8443${path}`,
        ], { stdio: "pipe" });
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
          await execa("kubectl", [
            "set", "env", `deployment/${name}`,
            "-n", ns, "-c", "inference-router",
            "EGRESS_LEARN_MODE=true",
          ], { stdio: "pipe" });
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
          await execa("kubectl", [
            "set", "env", `deployment/${name}`,
            "-n", ns, "-c", "inference-router",
            "EGRESS_LEARN_MODE=false",
          ], { stdio: "pipe" });
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
        }
        console.log(chalk.dim(`  Commands:`));
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
