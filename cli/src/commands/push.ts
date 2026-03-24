import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";
import path from "path";
import fs from "fs";
import { loadContext } from "../config.js";

export function pushCommand(): Command {
  const cmd = new Command("push");

  cmd
    .description("Build and push images to ACR (uses cached context from last deploy)")
    .option("--acr <name>", "ACR name (default: from last deploy)")
    .option("--only <image>", "Build only one image: controller, router, sandbox, relay, registry")
    .action(async (options) => {
      const { execa } = await import("execa");
      const blue = chalk.hex("#0078D4");

      // Resolve ACR from context or flag
      const ctx = loadContext();
      const acrName = options.acr || ctx?.acrName;
      const acrLoginServer = acrName ? `${acrName}.azurecr.io` : null;

      if (!acrName || !acrLoginServer) {
        console.error(chalk.red("\n  No ACR configured. Run 'azureclaw up' first or pass --acr <name>.\n"));
        process.exit(1);
      }

      // Find repo root (look for deploy/helm directory)
      let repoRoot = process.cwd();
      for (let i = 0; i < 5; i++) {
        if (fs.existsSync(path.join(repoRoot, "deploy", "helm"))) break;
        repoRoot = path.dirname(repoRoot);
      }
      if (!fs.existsSync(path.join(repoRoot, "deploy", "helm"))) {
        console.error(chalk.red("\n  Not in an AzureClaw repo. Run from the repo root.\n"));
        process.exit(1);
      }

      console.log(blue(`\n  AzureClaw · Push Images → ${acrLoginServer}\n`));

      // Login to ACR
      const spinner = ora("Logging into ACR...").start();
      try {
        await execa("az", ["acr", "login", "--name", acrName], { stdio: "pipe" });
        spinner.succeed("ACR login");
      } catch (e: any) {
        spinner.fail("ACR login failed");
        console.error(chalk.red(`  ${e.message}\n`));
        process.exit(1);
      }

      // Define all images
      const images: Array<{ name: string; tag: string; dockerfile: string; context?: string; buildArgs?: string[] }> = [
        { name: "controller", tag: "azureclaw-controller:latest", dockerfile: "controller/Dockerfile" },
        { name: "router", tag: "azureclaw-inference-router:latest", dockerfile: "inference-router/Dockerfile" },
        { name: "sandbox", tag: "openclaw-sandbox:latest", dockerfile: "sandbox-images/openclaw/Dockerfile",
          buildArgs: ["--build-arg", `INFERENCE_ROUTER_IMAGE=${acrLoginServer}/azureclaw-inference-router:latest`] },
        { name: "relay", tag: "agentmesh-relay:latest", dockerfile: "vendor/agentmesh-relay/Dockerfile", context: "vendor/agentmesh-relay" },
        { name: "registry", tag: "agentmesh-registry:latest", dockerfile: "vendor/agentmesh-registry/Dockerfile", context: "vendor/agentmesh-registry" },
      ];

      // Filter if --only specified
      const targets = options.only
        ? images.filter(i => i.name === options.only)
        : images;

      if (targets.length === 0) {
        console.error(chalk.red(`\n  Unknown image: ${options.only}. Options: controller, router, sandbox, relay, registry\n`));
        process.exit(1);
      }

      let failures = 0;
      for (const img of targets) {
        const spin = ora(`Building ${img.tag}...`).start();
        try {
          const args = [
            "build", "--platform", "linux/amd64",
            "--provenance=false", "--sbom=false",
            "-f", path.join(repoRoot, img.dockerfile),
            "-t", `${acrLoginServer}/${img.tag}`,
            ...(img.buildArgs || []),
            img.context ? path.join(repoRoot, img.context) : repoRoot,
          ];
          await execa("docker", args, { stdio: "pipe" });
          spin.text = `Pushing ${img.tag}...`;

          // Push with retry
          for (let attempt = 1; attempt <= 3; attempt++) {
            try {
              if (attempt > 1) await execa("az", ["acr", "login", "--name", acrName], { stdio: "pipe" });
              await execa("docker", ["push", `${acrLoginServer}/${img.tag}`], { stdio: "pipe" });
              break;
            } catch (e: any) {
              if (attempt === 3) throw e;
              spin.text = `Push ${img.tag} failed, retry ${attempt + 1}/3...`;
              await new Promise(r => setTimeout(r, 3000));
            }
          }
          spin.succeed(`${img.tag}`);
        } catch (e: any) {
          spin.fail(`${img.tag} — ${e.message?.split("\n")[0] || "failed"}`);
          failures++;
        }
      }

      if (failures > 0) {
        console.error(chalk.red(`\n  ${failures}/${targets.length} images failed.\n`));
        process.exit(1);
      }

      console.log(chalk.green(`\n  ✓ ${targets.length} image(s) pushed to ${acrLoginServer}\n`));

      if (ctx?.aksCluster) {
        console.log(chalk.dim(`  To redeploy: azureclaw up`));
        console.log(chalk.dim(`  To restart pods: kubectl rollout restart deployment -n azureclaw-system\n`));
      }
    });

  return cmd;
}
