import { Command } from "commander";
import { execa } from "execa";

export function logsCommand(): Command {
  const cmd = new Command("logs");

  cmd
    .description("Stream agent and platform logs")
    .argument("<name>", "Sandbox name")
    .option("-f, --follow", "Follow log output", false)
    .option("--tail <lines>", "Number of lines to show", "100")
    .action(async (name: string, options) => {
      const namespace = `azureclaw-${name}`;
      const args = [
        "logs",
        "-n",
        namespace,
        "-l",
        `azureclaw.azure.com/sandbox=${name}`,
        "--tail",
        options.tail,
      ];

      if (options.follow) {
        args.push("-f");
      }

      try {
        await execa("kubectl", args, { stdio: "inherit" });
      } catch {
        // Expected on disconnect
      }
    });

  return cmd;
}
