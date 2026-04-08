import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";
import { getAdminToken, withAdminAuth } from "../router-admin.js";

export function policyCommand(): Command {
  const cmd = new Command("policy");

  cmd.description("Manage sandbox network and security policies");

  cmd
    .command("allow")
    .description("Add an allowed egress endpoint to a running sandbox (hot-reload)")
    .argument("<name>", "Sandbox name")
    .argument("<host>", "Hostname to allow (e.g. api.github.com)")
    .option("--port <port>", "Port", "443")
    .action(async (name: string, host: string, options) => {
      const { execa } = await import("execa");
      const spinner = ora(`Allowing ${host}:${options.port} for '${name}'...`).start();

      try {
        // First get the current allowed endpoints
        const { stdout: current } = await execa("kubectl", [
          "get", "clawsandbox", name,
          "-n", "azureclaw-system",
          "-o", "jsonpath={.spec.networkPolicy.allowedEndpoints}",
        ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));

        const existing = current.trim() ? JSON.parse(current.trim()) : [];
        existing.push({ host, port: parseInt(options.port) });

        // Use merge patch to set the full array
        await execa("kubectl", [
          "patch", "clawsandbox", name,
          "-n", "azureclaw-system",
          "--type", "merge",
          "-p", JSON.stringify({ spec: { networkPolicy: { allowedEndpoints: existing } } }),
        ], { stdio: "pipe" });

        spinner.succeed(`${host}:${options.port} allowed for '${name}' (hot-reloaded)`);
        console.log(chalk.dim("  Controller will update NetworkPolicy within seconds.\n"));
      } catch (error) {
        spinner.fail("Failed to update policy");
        const message = error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
      }
    });

  cmd
    .command("get")
    .description("Show the active policy for a sandbox")
    .argument("<name>", "Sandbox name")
    .action(async (name: string) => {
      const { execa } = await import("execa");
      const ns = `azureclaw-${name}`;
      try {
        // Get NetworkPolicy from the sandbox namespace
        const { stdout } = await execa("kubectl", [
          "get", "networkpolicy", "sandbox-policy",
          "-n", ns,
          "-o", "json",
        ], { stdio: "pipe" });

        const policy = JSON.parse(stdout);
        const egress = policy.spec?.egress || [];

        console.log(chalk.bold(`\n  Policy for: ${name}\n`));
        console.log(`  Default:    ${chalk.red("deny all egress")}`);
        console.log(`  Allowed:\n`);

        for (const rule of egress) {
          const ports = (rule.ports || []).map((p: any) => `${p.protocol}/${p.port}`).join(", ");
          const targets = (rule.to || []).map((t: any) => {
            if (t.ipBlock) return `ipBlock: ${t.ipBlock.cidr}`;
            if (t.namespaceSelector) return "namespace: kube-system";
            return "?";
          }).join(" + ");
          console.log(`    ${chalk.green("allow")} ${ports} → ${targets}`);
        }
        console.log();
      } catch {
        console.log(chalk.red(`\n  Policy not found for '${name}'.\n`));
      }
    });

  cmd
    .command("deny")
    .description("Remove an allowed endpoint from a running sandbox")
    .argument("<name>", "Sandbox name")
    .argument("<host>", "Hostname to deny")
    .action(async (name: string, host: string) => {
      const { execa } = await import("execa");
      const spinner = ora(`Removing ${host} from '${name}' allowlist...`).start();

      try {
        const { stdout: current } = await execa("kubectl", [
          "get", "clawsandbox", name,
          "-n", "azureclaw-system",
          "-o", "jsonpath={.spec.networkPolicy.allowedEndpoints}",
        ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));

        const existing: Array<{ host: string; port?: number }> = current.trim() ? JSON.parse(current.trim()) : [];
        const filtered = existing.filter((ep) => ep.host !== host);

        if (filtered.length === existing.length) {
          spinner.warn(`${host} was not in the allowlist for '${name}'`);
          return;
        }

        await execa("kubectl", [
          "patch", "clawsandbox", name,
          "-n", "azureclaw-system",
          "--type", "merge",
          "-p", JSON.stringify({ spec: { networkPolicy: { allowedEndpoints: filtered } } }),
        ], { stdio: "pipe" });

        spinner.succeed(`${host} removed from '${name}' allowlist (hot-reloaded)`);
        console.log(chalk.dim("  Controller will update NetworkPolicy within seconds.\n"));
      } catch (error) {
        spinner.fail("Failed to update policy");
        const message = error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
      }
    });

  cmd
    .command("learn")
    .description("[DEPRECATED] Use 'azureclaw egress <name> --learned' instead")
    .argument("<name>", "Sandbox name")
    .option("--apply", "Apply learned domains as the sandbox allowlist", false)
    .option("--clear", "Clear learned domains after export", false)
    .action(async (name: string, options) => {
      console.log(chalk.yellow("\n  ⚠ 'policy learn' is deprecated. Use these instead:"));
      console.log(chalk.dim("    View learned domains:  azureclaw egress " + name + " --learned"));
      console.log(chalk.dim("    Apply as allowlist:    azureclaw egress " + name + " --enforce"));
      console.log(chalk.dim("    Approve a domain:      azureclaw egress " + name + " --approve <domain>\n"));

      // Still execute for backward compat
      const { execa } = await import("execa");
      const namespace = `azureclaw-${name}`;
      const spinner = ora(`Fetching learned domains from '${name}'...`).start();

      try {
        // Port-forward to the router and query /egress/learned
        const pod = await execa("kubectl", [
          "get", "pods", "-n", namespace,
          "-o", "jsonpath={.items[0].metadata.name}",
        ], { stdio: "pipe" });

        const podName = pod.stdout.trim();
        if (!podName) {
          spinner.fail(`No running pod found for '${name}'`);
          return;
        }

        // Use kubectl exec to curl the router from inside the pod (via inference-router container)
        const adminToken = await getAdminToken(namespace);
        const { stdout } = await execa("kubectl", [
          "exec", "-n", namespace, podName, "-c", "inference-router", "--",
          ...withAdminAuth(["curl", "-sf", "http://localhost:8443/egress/learned"], adminToken),
        ], { stdio: "pipe" });

        const data = JSON.parse(stdout);
        const domains: string[] = data.domains || [];

        spinner.succeed(`Learned ${domains.length} domains from '${name}'`);

        if (!data.learn_mode) {
          console.log(chalk.yellow("\n  ⚠ Learn mode is not active on this sandbox."));
          console.log(chalk.yellow("  Enable with: azureclaw add <name> --learn-egress\n"));
        }

        if (domains.length === 0) {
          console.log(chalk.dim("  No domains observed yet.\n"));
          return;
        }

        console.log(chalk.bold("\n  Learned domains (review before approving):\n"));
        for (const domain of domains) {
          console.log(`    ${chalk.green("+")} ${domain}`);
        }
        console.log();

        // Apply as allowlist if requested
        if (options.apply) {
          const applySpinner = ora("Applying learned domains as allowlist...").start();
          const endpoints = domains.map((d) => ({ host: d, port: 443 }));
          await execa("kubectl", [
            "patch", "clawsandbox", name,
            "-n", "azureclaw-system",
            "--type", "merge",
            "-p", JSON.stringify({
              spec: {
                networkPolicy: {
                  allowedEndpoints: endpoints,
                  learnEgress: false,
                },
              },
            }),
          ], { stdio: "pipe" });
          applySpinner.succeed("Allowlist applied + learn mode disabled");
          console.log(chalk.dim(`  ${domains.length} domains now in allowlist.`));
          console.log(chalk.dim("  Controller will reconcile within seconds.\n"));
        }

        // Clear learned domains if requested
        if (options.clear) {
          await execa("kubectl", [
            "exec", "-n", namespace, podName, "-c", "inference-router", "--",
            ...withAdminAuth(["curl", "-sf", "-X", "POST", "http://localhost:8443/egress/learned/clear"], adminToken),
          ], { stdio: "pipe" });
          console.log(chalk.dim("  Learned domains cleared.\n"));
        }
      } catch (error) {
        spinner.fail("Failed to fetch learned domains");
        const message = error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
      }
    });

  return cmd;
}
