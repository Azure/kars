import { Command } from "commander";
import { execa } from "execa";

export function traceCommand(): Command {
  const cmd = new Command("trace");

  cmd
    .description(
      "Live eBPF trace — see network calls, file access, and process execution in real time"
    )
    .argument("<name>", "Sandbox name")
    .option("--network", "Show network connections only", false)
    .option("--files", "Show file operations only", false)
    .option("--exec", "Show process executions only", false)
    .action(async (name: string, options) => {
      const namespace = `azureclaw-${name}`;
      const podLabel = `azureclaw.azure.com/sandbox=${name}`;

      // Build kubectl gadget args based on what the user wants to trace
      let gadget = "trace exec,open,tcp,dns"; // default: everything
      if (options.network) gadget = "trace tcp,dns";
      if (options.files) gadget = "trace open";
      if (options.exec) gadget = "trace exec";

      try {
        // TODO: Use Inspektor Gadget API/CLI to stream traces
        // For now, delegate to kubectl gadget
        await execa(
          "kubectl",
          [
            "gadget",
            gadget,
            "-n",
            namespace,
            "--podname",
            podLabel,
            "-o",
            "columns",
          ],
          { stdio: "inherit" }
        );
      } catch {
        // Expected on disconnect
      }
    });

  return cmd;
}
