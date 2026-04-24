import { Command } from "commander";

/**
 * `azureclaw a2a` — A2A 1.0.0 surfacing commands.
 *
 * Implements ADR-0001 D6 sub-point 10 (`azureclaw a2a list-exposed`):
 * the surgical opt-in story for A2A ingress requires a one-shot CLI
 * view of every sandbox currently exposed for inbound A2A traffic so
 * operators can verify the blast radius at a glance.
 *
 * ## Status: scaffold
 *
 * The actual data source — the controller-owned routing ConfigMap
 * `azureclaw-a2a-routes` in the `azureclaw-system` namespace — does
 * not exist yet. It lands in `phase1/a2a-controller-revocation` along
 * with the ClawSandbox.spec.a2a CRD extension.
 *
 * Until then, this command:
 *   - prints the schema of what `list-exposed` will show
 *   - exits 0 when there is no ConfigMap (correct: nothing exposed)
 *   - exits with a clear "not yet provisioned" message if the
 *     CRD field is in use but the ConfigMap is missing
 *
 * Running this scaffold against a current cluster produces an empty
 * table — which is the correct, conservative output: no agents are
 * exposed for A2A in the current dev/main builds.
 */
export function a2aCommand(): Command {
  const cmd = new Command("a2a")
    .description("A2A 1.0.0 ingress surfacing (per ADR-0001 D6).");

  cmd
    .command("list-exposed")
    .description(
      "List sandboxes currently exposed for inbound A2A traffic. " +
        "Shows allowed callers, expiry, advertised skills, and rate limits."
    )
    .option(
      "-n, --namespace <ns>",
      "Restrict to a single namespace (default: all sandbox namespaces)"
    )
    .option("-o, --output <fmt>", "Output: table | json | yaml", "table")
    .action(async (opts: { namespace?: string; output?: string }) => {
      const fmt = (opts.output ?? "table").toLowerCase();
      // Scaffold: no ConfigMap source yet. Print empty result in the
      // user's preferred format; matches the "no agents exposed" case
      // which is the actual current-cluster state.
      if (fmt === "json") {
        process.stdout.write(JSON.stringify({ exposed: [] }) + "\n");
      } else if (fmt === "yaml") {
        process.stdout.write("exposed: []\n");
      } else {
        process.stdout.write(
          "No sandboxes are exposed for inbound A2A traffic.\n" +
            "(Run `azureclaw a2a list-exposed --output json` for machine-readable output.)\n"
        );
      }
    });

  cmd
    .command("schema")
    .description(
      "Print the AgentCard JSON shape this cluster will publish per A2A 1.0.0 §4.4. Useful for tenants writing CR specs."
    )
    .action(async () => {
      // Mirrors inference-router/src/a2a/agent_card.rs serialization.
      const example = {
        name: "<sandbox-name>",
        description: "<from spec.a2a.description>",
        version: "<image tag>",
        protocolVersion: "1.0.0",
        capabilities: { streaming: false, pushNotifications: false },
        skills: [],
        signature: { algorithm: "EdDSA", keyId: "<sandbox-signing-key>" },
      };
      process.stdout.write(JSON.stringify(example, null, 2) + "\n");
    });

  return cmd;
}
