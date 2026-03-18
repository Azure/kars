import { Command } from "commander";
import { execa } from "execa";

export function connectCommand(): Command {
  const cmd = new Command("connect");

  cmd
    .description("Open an interactive shell inside a sandbox pod")
    .argument("<name>", "Sandbox name")
    .action(async (name: string) => {
      const namespace = `azureclaw-${name}`;
      const podLabel = `azureclaw.azure.com/sandbox=${name}`;

      try {
        await execa("kubectl", [
          "exec",
          "-it",
          "-n",
          namespace,
          "-l",
          podLabel,
          "--",
          "/bin/bash",
        ], { stdio: "inherit" });
      } catch {
        // kubectl exec returns non-zero on disconnect — that's expected
      }
    });

  return cmd;
}
