// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Slice 1c.6 — `kars policy sign --kind X` unified signing CLI.
//
// Single dispatch surface that supersedes the egress-only signing flow
// inside `kars egress`. Operators authoring any of the five signed
// policy artifact kinds (egress-allowlist | agt-profile |
// inference-policy | memory-binding | mcp-server-bundle) point this at
// a prebuilt canonical-form file on disk; the command pushes the bytes
// to an OCI registry under the per-kind media type, signs the manifest
// digest with cosign, and (optionally) emits the matching `bundleRef`
// snippet for the operator to paste into the consuming CRD.
//
// **Canonical form is the operator's responsibility.** The controller's
// `policy_canonical::*::parse` is the authoritative byte-level
// validator (per the canonical-format doc in
// `docs/internal/crd-well-oiled-machine/policy-canonical-format.md`).
// The CLI does only cheap pre-flight (file exists, non-empty UTF-8) and
// trusts the controller to reject malformed bytes post-fetch with a
// crisp `Degraded / SpecInvalid` condition.
//
// The egress-allowlist kind also supports the canonical-builder flow
// (see `kars egress` — that command builds the canonical YAML from
// CR-side `allowedEndpoints` and then calls into this signing pipeline
// internally). This top-level command is for the other four kinds
// where the operator already has the bytes.

import { Command } from "commander";
import { readFileSync } from "node:fs";
import { basename } from "node:path";

import {
  EGRESS_ALLOWLIST_MEDIA_TYPE,
  autoDetectSignMode,
  pushArtifact,
  signArtifact,
  type SignMode,
} from "../egress/sign.js";

/** Identifier accepted on the `--kind` flag. Must mirror the controller
 *  `policy_canonical::*::KIND` constants for clarity in operator UX, but
 *  the CLI uses lowercase-hyphenated form (industry convention for
 *  flag values) — the wire-level media type is what actually flows on
 *  the registry, so the discriminator is unambiguous. */
export type PolicyKindId =
  | "egress-allowlist"
  | "agt-profile"
  | "inference-policy"
  | "memory-binding"
  | "mcp-server-bundle"
  | "eval-corpus";

export const POLICY_KIND_IDS: readonly PolicyKindId[] = [
  "egress-allowlist",
  "agt-profile",
  "inference-policy",
  "memory-binding",
  "mcp-server-bundle",
  "eval-corpus",
] as const;

/** Wire-level metadata for one signed-artifact kind. The MIME types
 *  here MUST match the controller's `MEDIA_TYPE` consts byte-for-byte
 *  (see `controller/src/policy_canonical/*.rs`). */
export interface PolicyKindSpec {
  /** CLI flag value, e.g. `egress-allowlist`. */
  id: PolicyKindId;
  /** Controller-side CRD-kind constant (display only). */
  controllerKind: string;
  /** OCI artifactType for the registry push + cosign verify. */
  mediaType: string;
  /** Default file extension for the canonical bytes (display only). */
  expectedExt: string;
}

export const POLICY_KIND_SPECS: Record<PolicyKindId, PolicyKindSpec> = {
  "egress-allowlist": {
    id: "egress-allowlist",
    controllerKind: "EgressAllowlist",
    mediaType: EGRESS_ALLOWLIST_MEDIA_TYPE,
    expectedExt: ".yaml",
  },
  "agt-profile": {
    id: "agt-profile",
    controllerKind: "AgtProfile",
    mediaType: "application/vnd.kars.agt-profile.v1+yaml",
    expectedExt: ".yaml",
  },
  "inference-policy": {
    id: "inference-policy",
    controllerKind: "InferencePolicy",
    mediaType: "application/vnd.kars.inference-policy.v1+json",
    expectedExt: ".json",
  },
  "memory-binding": {
    id: "memory-binding",
    controllerKind: "KarsMemory",
    mediaType: "application/vnd.kars.memory-binding.v1+json",
    expectedExt: ".json",
  },
  "mcp-server-bundle": {
    id: "mcp-server-bundle",
    controllerKind: "McpServer",
    mediaType: "application/vnd.kars.mcp-server-bundle.v1+json",
    expectedExt: ".json",
  },
  "eval-corpus": {
    id: "eval-corpus",
    controllerKind: "KarsEval",
    mediaType: "application/vnd.kars.eval-corpus.v1+json",
    expectedExt: ".json",
  },
};

export function lookupPolicyKindSpec(id: string): PolicyKindSpec {
  const spec = (POLICY_KIND_SPECS as Record<string, PolicyKindSpec | undefined>)[id];
  if (!spec) {
    throw new Error(
      `unknown --kind '${id}'. valid kinds: ${POLICY_KIND_IDS.join(", ")}`,
    );
  }
  return spec;
}

/** Result returned by [`signPolicyArtifact`]; suitable for JSON output
 *  + bundleRef-snippet generation. */
export interface SignedPolicyArtifact {
  kind: PolicyKindId;
  mediaType: string;
  registry: string;
  repository: string;
  tag: string;
  digest: string;
  signMode: SignMode;
}

export interface SignPolicyArtifactOpts {
  kind: PolicyKindId;
  filePath: string;
  registry: string;
  repository: string;
  tag?: string;
  signMode?: string;
  signKey?: string;
  /** Override for tests. */
  orasPath?: string;
  /** Override for tests. */
  cosignPath?: string;
  /** Override for tests. */
  isTTY?: boolean;
  /** Override for tests. */
  env?: NodeJS.ProcessEnv;
  /** Override for tests — when set, `pushArtifact` is bypassed and
   *  this digest is treated as authoritative. */
  digestOverride?: string;
  /** Override for tests — when true, `signArtifact` is bypassed. */
  skipSign?: boolean;
}

/**
 * Pre-flight a kind+file pair and run the full push → sign pipeline.
 * Pure-ish — every spawn is overridable for tests.
 *
 * The canonical-form validation responsibility is **deliberately not
 * here**. The controller's `policy_canonical::*::parse` is the
 * authoritative checker; bytes the operator wants to ship are
 * pushed as-is. The CLI catches only the gross hygiene errors
 * (missing file, empty file, non-UTF-8 bytes) so we don't waste a
 * Fulcio cert on something the controller will reject in 5 seconds.
 */
export async function signPolicyArtifact(
  opts: SignPolicyArtifactOpts,
): Promise<SignedPolicyArtifact> {
  const spec = lookupPolicyKindSpec(opts.kind);

  // Pre-flight: read bytes in one shot (no check-then-use TOCTOU
  // window). Map ENOENT/EISDIR/etc. errno strings to a single
  // operator-friendly message; everything else surfaces verbatim.
  let buf: Buffer;
  try {
    buf = readFileSync(opts.filePath);
  } catch (e) {
    const err = e as NodeJS.ErrnoException;
    if (err.code === "ENOENT") {
      throw new Error(`--file '${opts.filePath}' does not exist`);
    }
    if (err.code === "EISDIR") {
      throw new Error(`--file '${opts.filePath}' is not a regular file`);
    }
    throw new Error(`--file '${opts.filePath}': ${err.message}`);
  }
  if (buf.length === 0) {
    throw new Error(`--file '${opts.filePath}' is empty`);
  }
  // Verify bytes are valid UTF-8 (Buffer→string with strict decode
  // catches embedded NULs + invalid surrogates). All five canonical
  // formats are UTF-8 text — JSON, YAML, or YAML-with-comments.
  try {
    new TextDecoder("utf-8", { fatal: true }).decode(buf);
  } catch (e) {
    throw new Error(
      `--file '${opts.filePath}' is not valid UTF-8: ${(e as Error).message}`,
    );
  }
  const bytes = buf.toString("utf8");

  // Sign-mode auto-detect: respects user-explicit override, env hints,
  // TTY presence — identical heuristic to `kars egress --sign`.
  const env = opts.env ?? process.env;
  const isTTY = opts.isTTY ?? Boolean(process.stdin?.isTTY);
  const signMode = autoDetectSignMode({
    signModeFlag: opts.signMode,
    signKey: opts.signKey,
    isTTY,
    env,
  });

  const tag = opts.tag ?? "latest";

  // Push.
  let digest: string;
  if (opts.digestOverride) {
    digest = opts.digestOverride;
  } else {
    digest = await pushArtifact({
      orasPath: opts.orasPath ?? "oras",
      registry: opts.registry,
      repository: opts.repository,
      yaml: bytes,
      artifactType: spec.mediaType,
      tag,
    });
  }
  if (!digest.startsWith("sha256:")) {
    throw new Error(`pushed digest is not a sha256: ${digest}`);
  }

  // Sign.
  if (!opts.skipSign) {
    await signArtifact({
      cosignPath: opts.cosignPath ?? "cosign",
      registry: opts.registry,
      repository: opts.repository,
      digest,
      mode: signMode,
      keyRef: opts.signKey,
    });
  }

  return {
    kind: opts.kind,
    mediaType: spec.mediaType,
    registry: opts.registry,
    repository: opts.repository,
    tag,
    digest,
    signMode,
  };
}

/** Render a copy-pasteable `bundleRef` snippet for the operator's CRD.
 *  Returns a multi-line YAML fragment (no trailing newline) that
 *  embeds under `spec.<field>` of the consuming kind. */
export function renderBundleRefSnippet(artifact: SignedPolicyArtifact): string {
  return [
    "bundleRef:",
    `  registry: ${artifact.registry}`,
    `  repository: ${artifact.repository}`,
    `  tag: ${artifact.tag}`,
    `  digest: ${artifact.digest}`,
  ].join("\n");
}

/** Register `kars policy sign` on an existing `Command`
 *  (typically the `policy` parent created by
 *  `cli/src/commands/policy.ts`). */
export function registerPolicySignSubcommand(parent: Command): Command {
  return parent
    .command("sign")
    .description(
      "Sign a policy artifact (egress-allowlist, agt-profile, inference-policy, memory-binding, mcp-server-bundle, eval-corpus)",
    )
    .requiredOption(
      "--kind <kind>",
      `Artifact kind. One of: ${POLICY_KIND_IDS.join(", ")}`,
    )
    .requiredOption("--file <path>", "Path to canonical-form artifact bytes")
    .requiredOption("--registry <host>", "OCI registry hostname (e.g. myacr.azurecr.io)")
    .requiredOption("--repository <repo>", "OCI repository path under the registry")
    .option("--tag <tag>", "Tag to push under (informational; cosign signs by digest)", "latest")
    .option("--sign-mode <mode>", "cosign mode: keyless | identity-token | keyed")
    .option("--sign-key <ref>", "cosign key reference (required for --sign-mode keyed)")
    .option("--json", "Emit a JSON envelope instead of human-readable output")
    .option("--print-bundle-ref", "Print a YAML bundleRef snippet ready to paste into the consuming CRD", false)
    .action(async (raw: Record<string, unknown>) => {
      try {
        const kind = String(raw.kind);
        const result = await signPolicyArtifact({
          kind: lookupPolicyKindSpec(kind).id,
          filePath: String(raw.file),
          registry: String(raw.registry),
          repository: String(raw.repository),
          tag: raw.tag ? String(raw.tag) : undefined,
          signMode: raw.signMode ? String(raw.signMode) : undefined,
          signKey: raw.signKey ? String(raw.signKey) : undefined,
        });

        if (raw.json) {
          process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
        } else {
          process.stdout.write(`signed ${basename(String(raw.file))} as ${result.kind}\n`);
          process.stdout.write(`  artifactType: ${result.mediaType}\n`);
          process.stdout.write(`  digest: ${result.digest}\n`);
          process.stdout.write(`  mode:   ${result.signMode}\n`);
        }

        if (raw.printBundleRef) {
          process.stdout.write(`\n${renderBundleRefSnippet(result)}\n`);
        }
      } catch (e) {
        process.stderr.write(`kars policy sign: ${(e as Error).message}\n`);
        process.exitCode = 1;
      }
    });
}
