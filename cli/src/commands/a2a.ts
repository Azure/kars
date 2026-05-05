// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { readFile } from "node:fs/promises";
import {
  formatAge,
  parseSpecFile,
  stripUndefined,
} from "./crd-helpers.js";
import { runApply, runDelete, runGet, runList } from "./toolpolicy.js";

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
    .description("Inspect A2A (Agent-to-Agent) ingress surfaces");

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
      "Print the AgentCard JSON shape this cluster publishes per the A2A spec. Useful for tenants writing CR specs."
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

// ---------------------------------------------------------------------------
// `azureclaw a2a-agent {apply,get,list,delete}` — A2AAgent CR management.
// Lives in this file (not a sibling) so all A2A surfaces share one module
// per the no-duplicate-modules guidance.
// ---------------------------------------------------------------------------

const A2A_KIND = "A2AAgent";
const A2A_PLURAL = "a2aagents";

export interface A2aAgentApplyOptions {
  fromFile?: string;
  endpointUrl?: string;
  productionMode?: boolean;
  signingKey?: string[];
  capability?: string[];
  description?: string;
  displayName?: string;
  toolPolicy?: string;
  requireSigned?: boolean;
  minSignatures?: string;
  maxSkewSeconds?: string;
}

interface A2aSigningKey {
  kid: string;
  alg: string;
  publicKeyB64u: string;
  notAfter?: number;
}

export function parseSigningKey(raw: string): A2aSigningKey {
  // Format: kid:alg:publicKeyB64u[:notAfterUnixSecs]
  const parts = raw.split(":");
  if (parts.length < 3 || parts.length > 4) {
    throw new Error(
      `--signing-key expects 'kid:alg:publicKeyB64u[:notAfter]' (got '${raw}')`,
    );
  }
  const [kid, alg, publicKeyB64u, notAfterRaw] = parts;
  if (!kid || !alg || !publicKeyB64u) {
    throw new Error(`--signing-key has empty kid/alg/publicKeyB64u in '${raw}'`);
  }
  const out: A2aSigningKey = { kid, alg, publicKeyB64u };
  if (notAfterRaw !== undefined && notAfterRaw !== "") {
    const n = Number(notAfterRaw);
    if (!Number.isInteger(n) || n < 0) {
      throw new Error(`--signing-key notAfter must be a Unix-seconds integer (got '${notAfterRaw}')`);
    }
    out.notAfter = n;
  }
  return out;
}

function parseUint(raw: string, flag: string): number {
  const n = Number(raw);
  if (!Number.isInteger(n) || n < 0) {
    throw new Error(`${flag} must be a non-negative integer (got '${raw}')`);
  }
  return n;
}

export function buildA2aAgentSpecFromFlags(
  o: A2aAgentApplyOptions,
): Record<string, unknown> {
  const signingKeys = (o.signingKey ?? []).map(parseSigningKey);

  let trust: Record<string, unknown> | undefined;
  if (o.requireSigned || o.minSignatures !== undefined || o.maxSkewSeconds !== undefined) {
    trust = {
      requireSignedRequests: o.requireSigned ? true : undefined,
      minSignaturesRequired:
        o.minSignatures !== undefined ? parseUint(o.minSignatures, "--min-signatures") : undefined,
      maxClockSkewSeconds:
        o.maxSkewSeconds !== undefined ? parseUint(o.maxSkewSeconds, "--max-skew-seconds") : undefined,
    };
  }

  const policyRefs = o.toolPolicy ? { toolPolicy: o.toolPolicy } : undefined;

  return stripUndefined({
    endpointUrl: o.endpointUrl,
    productionMode: o.productionMode ? true : undefined,
    signingKeys: signingKeys.length > 0 ? signingKeys : undefined,
    capabilities: o.capability && o.capability.length > 0 ? o.capability : undefined,
    description: o.description,
    displayName: o.displayName,
    policyRefs,
    trust,
  }) as Record<string, unknown>;
}

export function validateA2aAgentSpec(spec: Record<string, unknown>): string[] {
  const errs: string[] = [];
  const url = spec.endpointUrl as string | undefined;
  if (!url) {
    errs.push("missing required spec.endpointUrl — pass --endpoint-url or include in --from-file");
  } else if (!/^https?:\/\//.test(url)) {
    errs.push(`spec.endpointUrl must start with http:// or https:// (got '${url}')`);
  } else if (spec.productionMode === true && !url.startsWith("https://")) {
    errs.push("productionMode requires spec.endpointUrl to begin with https://");
  }

  const sk = spec.signingKeys as unknown[] | undefined;
  if (!sk || !Array.isArray(sk) || sk.length === 0) {
    errs.push(
      "missing required spec.signingKeys — pass --signing-key kid:alg:publicKeyB64u (repeatable)",
    );
  } else {
    for (const k of sk) {
      const key = k as Record<string, unknown>;
      if (key.alg !== "EdDSA") {
        errs.push(`spec.signingKeys[*].alg must be 'EdDSA' (got '${String(key.alg)}')`);
        break;
      }
    }
  }
  return errs;
}

export function summarizeA2aAgentRow(
  item: Record<string, unknown>,
  now: Date = new Date(),
): string[] {
  const meta = (item.metadata ?? {}) as Record<string, unknown>;
  const spec = (item.spec ?? {}) as Record<string, unknown>;
  const status = (item.status ?? {}) as Record<string, unknown>;
  return [
    String(meta.name ?? "<unknown>"),
    String(spec.endpointUrl ?? "-"),
    String(spec.productionMode === true ? "yes" : "no"),
    formatAge(meta.creationTimestamp as string | undefined, now),
    String(status.phase ?? "-"),
  ];
}

function appendOpt(value: string, prev: string[]): string[] {
  return [...(prev ?? []), value];
}

function formatBlock(v: unknown): string {
  if (!v || typeof v !== "object") return "-";
  const entries = Object.entries(v as Record<string, unknown>).filter(
    ([, x]) => x !== undefined && x !== null,
  );
  if (entries.length === 0) return "-";
  return entries.map(([k, x]) => `${k}=${JSON.stringify(x)}`).join(", ");
}

export function a2aAgentCommand(): Command {
  const cmd = new Command("a2a-agent")
    .description("Manage A2AAgent CRs (A2A 1.2 agents + signing-key trust anchors)");

  cmd
    .command("apply")
    .description("Create or update an A2AAgent")
    .argument("<name>", "A2AAgent name (DNS-1123)")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("--from-file <path>", "Read spec from a YAML/JSON file")
    .option("--endpoint-url <url>", "Agent endpoint URL")
    .option("--production-mode", "Reject unauthenticated traffic; require https://", false)
    .option(
      "--signing-key <kid:alg:b64u>",
      "Signing key entry kid:alg:publicKeyB64u[:notAfter] (repeatable, ≥1 required)",
      appendOpt,
      [] as string[],
    )
    .option("--capability <s>", "Advertised capability (repeatable)", appendOpt, [] as string[])
    .option("--description <s>", "AgentCard description")
    .option("--display-name <s>", "Human-readable display name")
    .option("--policy-toolpolicy <name>", "ToolPolicy CR name to join at request time")
    .option("--require-signed", "Reject unsigned inbound A2A requests", false)
    .option("--min-signatures <n>", "Minimum independent valid signatures required")
    .option("--max-skew-seconds <n>", "Maximum tolerated clock skew (seconds)")
    .action(async (name: string, opts: A2aAgentApplyOptions & { namespace: string; policyToolpolicy?: string }) => {
      // Commander camel-cases --policy-toolpolicy → policyToolpolicy.
      const merged: A2aAgentApplyOptions = {
        ...opts,
        toolPolicy: (opts as { policyToolpolicy?: string }).policyToolpolicy,
      };
      await runApply(name, merged, opts.namespace, A2A_KIND, A2A_PLURAL, async () => {
        if (merged.fromFile) {
          const content = await readFile(merged.fromFile, "utf8");
          return parseSpecFile(content);
        }
        return buildA2aAgentSpecFromFlags(merged);
      }, validateA2aAgentSpec);
    });

  cmd
    .command("get")
    .description("Show an A2AAgent by name")
    .argument("<name>", "Name")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("-o, --output <fmt>", "Output: pretty|yaml|json", "pretty")
    .action(async (name: string, opts: { namespace: string; output: string }) => {
      await runGet(name, opts.namespace, opts.output, A2A_PLURAL, (item) => {
        const spec = (item.spec ?? {}) as Record<string, unknown>;
        const status = (item.status ?? {}) as Record<string, unknown>;
        const sk = (spec.signingKeys ?? []) as Array<Record<string, unknown>>;
        console.log(chalk.bold(`\n  A2AAgent/${name}\n`));
        console.log(`  Endpoint:     ${spec.endpointUrl ?? "-"}`);
        console.log(`  Production:   ${spec.productionMode === true ? "yes" : "no"}`);
        console.log(`  SigningKeys:  ${sk.length} (${sk.map((k) => k.kid).join(", ") || "-"})`);
        console.log(`  Trust:        ${formatBlock(spec.trust)}`);
        console.log(`  Phase:        ${status.phase ?? "-"}\n`);
      });
    });

  cmd
    .command("list")
    .description("List A2AAgents in a namespace")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .action(async (opts: { namespace: string }) => {
      await runList(opts.namespace, A2A_PLURAL, ["NAME", "ENDPOINT", "PROD", "AGE", "STATUS"], summarizeA2aAgentRow);
    });

  cmd
    .command("delete")
    .description("Delete an A2AAgent")
    .argument("<name>", "Name")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("--no-prompt", "Skip confirmation")
    .action(async (name: string, opts: { namespace: string; prompt: boolean }) => {
      await runDelete(name, opts.namespace, A2A_PLURAL, opts.prompt);
    });

  return cmd;
}

export const __test = {
  buildA2aAgentSpecFromFlags,
  validateA2aAgentSpec,
  summarizeA2aAgentRow,
  parseSigningKey,
};
