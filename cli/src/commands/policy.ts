import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

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
      const namespace = `azureclaw-${name}`;

      try {
        // Patch the ClawSandbox CR to add the endpoint
        await execa("kubectl", [
          "patch", "clawsandbox", name,
          "-n", "azureclaw-system",
          "--type", "json",
          "-p", JSON.stringify([{
            op: "add",
            path: "/spec/networkPolicy/allowedEndpoints/-",
            value: { host, port: parseInt(options.port) }
          }]),
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
      const namespace = `azureclaw-${name}`;

      try {
        // Get NetworkPolicy from the sandbox namespace
        const { stdout } = await execa("kubectl", [
          "get", "networkpolicy", "sandbox-policy",
          "-n", namespace,
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
      console.log(chalk.yellow(`\n  Removing ${host} from '${name}' allowlist...`));
      console.log(chalk.dim("  TODO: patch CRD to remove endpoint, controller re-reconciles.\n"));
    });

  return cmd;
}
