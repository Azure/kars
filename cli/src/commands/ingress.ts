import { Command } from "commander";
import chalk from "chalk";
import { getAdminToken, withAdminAuth } from "../router-admin.js";

export function ingressCommand(): Command {
  const cmd = new Command("ingress");

  cmd
    .description("Manage inter-agent ingress: block/allow agents and trust threshold")
    .argument("[name]", "Sandbox name (default: demo-agent)", "demo-agent")
    .option("--namespace <ns>", "Kubernetes namespace")
    .option("--block <agent>", "Block an agent from communicating with this sandbox")
    .option("--unblock <agent>", "Unblock a previously blocked agent")
    .option("--allow <agent>", "Explicitly allow an agent (bypasses trust threshold)")
    .option("--blocked", "Show blocked agents")
    .option("--allowed", "Show explicitly allowed agents")
    .option("--agents", "Show all known agents with trust scores and ACL status")
    .option("--threshold <n>", "Set the trust threshold for KNOCK enforcement (0–1000)")
    .option("--status", "Show ingress status overview")
    .action(async (name: string, options) => {
      const { execa } = await import("execa");

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
        const curlArgs = mode === "docker"
          ? ["exec", containerName, "curl", "-s", `http://127.0.0.1:8443${path}`]
          : ["exec", "-n", ns, pod, "-c", "inference-router", "--",
             ...withAdminAuth(["curl", "-s", `http://127.0.0.1:8443${path}`], adminToken)];
        const bin = mode === "docker" ? "docker" : "kubectl";
        const { stdout } = await execa(bin, curlArgs, { stdio: "pipe" });
        return JSON.parse(stdout);
      }

      async function routerPost(path: string, body: object): Promise<any> {
        const curlArgs = mode === "docker"
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

      // Block an agent
      if (options.block) {
        try {
          const result = await routerPost("/ingress/block", { agent_id: options.block });
          console.log(chalk.red(`\n  🚫 Blocked: ${result.agent_id}`));
          console.log(chalk.dim(`     Agent will be rejected from communicating with this sandbox.\n`));
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to block agent: ${e.message}\n`));
        }
        return;
      }

      // Unblock an agent
      if (options.unblock) {
        try {
          const result = await routerPost("/ingress/unblock", { agent_id: options.unblock });
          if (result.was_blocked) {
            console.log(chalk.green(`\n  ✅ Unblocked: ${result.agent_id}\n`));
          } else {
            console.log(chalk.yellow(`\n  Agent '${result.agent_id}' was not blocked.\n`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to unblock agent: ${e.message}\n`));
        }
        return;
      }

      // Explicitly allow an agent
      if (options.allow) {
        try {
          const result = await routerPost("/ingress/allow", { agent_id: options.allow });
          console.log(chalk.green(`\n  ✅ Allowed: ${result.agent_id}`));
          console.log(chalk.dim(`     Agent will bypass the trust threshold for this sandbox.\n`));
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to allow agent: ${e.message}\n`));
        }
        return;
      }

      // Set trust threshold
      if (options.threshold !== undefined) {
        try {
          const threshold = parseInt(options.threshold, 10);
          if (isNaN(threshold) || threshold < 0 || threshold > 1000) {
            console.log(chalk.red(`\n  Threshold must be a number between 0 and 1000.\n`));
            return;
          }
          const result = await routerPost("/ingress/threshold", { threshold });
          console.log(chalk.green(`\n  🔒 Trust threshold updated: ${result.old_threshold} → ${result.new_threshold}`));
          if (result.new_threshold === 0) {
            console.log(chalk.dim(`     Threshold 0 = accept all agents (no KNOCK enforcement).\n`));
          } else {
            console.log(chalk.dim(`     Agents with trust score below ${result.new_threshold} will be rejected.\n`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to update threshold: ${e.message}\n`));
        }
        return;
      }

      // Show blocked agents
      if (options.blocked) {
        try {
          const data = await routerGet("/ingress/blocked");
          console.log(chalk.hex("#0078D4")(`\n  Blocked Agents for '${name}'`));
          if (data.agents && data.agents.length > 0) {
            console.log();
            for (const agent of data.agents) {
              console.log(`    ${chalk.red("🚫")} ${chalk.white(agent)}`);
            }
            console.log(chalk.dim(`\n  ${data.count} agent(s) blocked.\n`));
          } else {
            console.log(chalk.dim(`\n    No agents blocked.\n`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to query blocked agents: ${e.message}\n`));
        }
        return;
      }

      // Show allowed agents
      if (options.allowed) {
        try {
          const data = await routerGet("/ingress/allowed");
          console.log(chalk.hex("#0078D4")(`\n  Explicitly Allowed Agents for '${name}'`));
          if (data.agents && data.agents.length > 0) {
            console.log();
            for (const agent of data.agents) {
              console.log(`    ${chalk.green("✓")} ${agent}`);
            }
            console.log(chalk.dim(`\n  ${data.count} agent(s) explicitly allowed.\n`));
          } else {
            console.log(chalk.dim(`\n    No agents explicitly allowed.\n`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to query allowed agents: ${e.message}\n`));
        }
        return;
      }

      // Show all known agents with trust + ACL
      if (options.agents) {
        try {
          const data = await routerGet("/ingress/agents");
          console.log(chalk.hex("#0078D4")(`\n  Known Agents for '${name}'`));
          console.log(chalk.dim(`  Trust threshold: ${data.trust_threshold}\n`));
          if (data.agents && data.agents.length > 0) {
            for (const agent of data.agents) {
              const icon = agent.acl === "blocked" ? chalk.red("🚫")
                : agent.acl === "allowed" ? chalk.green("✓")
                : chalk.dim("·");
              const scoreColor = agent.score >= data.trust_threshold ? chalk.green : chalk.yellow;
              const aclLabel = agent.acl !== "default" ? chalk.dim(` [${agent.acl}]`) : "";
              console.log(`    ${icon} ${chalk.white(agent.agent_id)}  ${scoreColor(`score=${agent.score}`)}  ${chalk.dim(`tier=${agent.tier}`)}${aclLabel}`);
            }
            console.log(chalk.dim(`\n  ${data.count} agent(s) known.\n`));
          } else {
            console.log(chalk.dim(`    No agents known yet.\n`));
          }
        } catch (e: any) {
          console.log(chalk.red(`\n  Failed to query agents: ${e.message}\n`));
        }
        return;
      }

      // Default: show status
      try {
        const [status, blocked, allowed, agents] = await Promise.all([
          routerGet("/ingress/status"),
          routerGet("/ingress/blocked"),
          routerGet("/ingress/allowed"),
          routerGet("/ingress/agents").catch(() => ({ agents: [], count: 0 })),
        ]);
        console.log(chalk.hex("#0078D4")(`\n  Ingress Security — '${name}'`));
        console.log(`    Trust threshold: ${status.trust_threshold === 0 ? chalk.dim("0 (accept all)") : chalk.white(status.trust_threshold)}`);
        console.log(`    Known agents:   ${chalk.white(status.known_agents)}`);
        console.log(`    Blocked:        ${blocked.count > 0 ? chalk.red(blocked.count + " agent(s)") : chalk.dim("none")}`);
        console.log(`    Allowed:        ${allowed.count > 0 ? chalk.green(allowed.count + " agent(s)") : chalk.dim("none")}`);
        console.log();

        if (blocked.agents && blocked.agents.length > 0) {
          console.log(chalk.dim(`  Blocked agents:`));
          for (const agent of blocked.agents) {
            console.log(`    ${chalk.red("🚫")} ${agent}`);
          }
          console.log();
        }

        if (allowed.agents && allowed.agents.length > 0) {
          console.log(chalk.dim(`  Explicitly allowed agents:`));
          for (const agent of allowed.agents) {
            console.log(`    ${chalk.green("✓")} ${agent}`);
          }
          console.log();
        }

        console.log(chalk.dim(`  Commands:`));
        console.log(chalk.dim(`    azureclaw ingress ${name} --agents                Show all known agents`));
        console.log(chalk.dim(`    azureclaw ingress ${name} --block <agent>         Block an agent`));
        console.log(chalk.dim(`    azureclaw ingress ${name} --unblock <agent>       Unblock an agent`));
        console.log(chalk.dim(`    azureclaw ingress ${name} --allow <agent>         Explicitly allow an agent`));
        console.log(chalk.dim(`    azureclaw ingress ${name} --blocked               Show blocked agents`));
        console.log(chalk.dim(`    azureclaw ingress ${name} --allowed               Show allowed agents`));
        console.log(chalk.dim(`    azureclaw ingress ${name} --threshold <n>         Set trust threshold (0–1000)`));
        console.log();
      } catch (e: any) {
        console.log(chalk.red(`\n  Failed to query status: ${e.message}\n`));
      }
    });

  return cmd;
}
