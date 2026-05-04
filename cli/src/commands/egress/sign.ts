// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Phase 2 S12.c — `azureclaw egress … --sign` helpers.
//
// Producer side of the signed-egress-allowlist supply chain. Builds a
// canonical YAML artifact that is byte-identical to the spec in
// `docs/internal/policy-canonical-format.md`, pushes it as an OCI artifact via
// `oras`, signs the resulting digest with `cosign`, and patches the
// `ClawSandbox.spec.networkPolicy.allowlistRef` field.
//
// The canonical YAML serializer is deliberately written by hand (rather
// than reusing `js-yaml`/`yaml` defaults) because the S12.a spec is
// stricter than any general-purpose YAML emitter — block style only,
// fixed key order, IDNA-2008 host normalization, lexicographic endpoint
// sort, no anchors/aliases/comments, LF-only with trailing newline.
//
// This is the producer side of the consumer/validator that lands in
// S12.b. The on-CR allowlist remains non-authoritative in this slice
// (S12.e flips authority); the inline `allowedEndpoints` MUST stay
// byte-equivalent to the artifact contents until then.

import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const EGRESS_ALLOWLIST_MEDIA_TYPE =
  "application/vnd.azureclaw.egress-allowlist.v1+yaml";

export interface CanonicalEndpoint {
  host: string;
  port: number;
  protocol?: string;
}

export interface CanonicalAllowlistInput {
  generation: number;
  endpoints: Array<{ host: string; port: number; protocol?: string }>;
}

export interface CanonicalAllowlistOutput {
  yaml: string;
  endpoints: CanonicalEndpoint[];
}

export type SignMode = "keyless" | "identity-token" | "keyed";

const HOST_ASCII_RE = /^[a-z0-9.-]+$/;

/**
 * Normalize a single host per S12.a rule #7. Returns canonical
 * lowercase ASCII (Punycode for non-ASCII inputs). Throws on
 * uppercase ASCII, whitespace, control bytes, wildcards, leading or
 * trailing dots, consecutive dots, or any byte outside `[a-z0-9.-]`
 * after IDNA conversion.
 */
function normalizeHost(rawHost: string): string {
  if (typeof rawHost !== "string" || rawHost.length === 0) {
    throw new Error(`canonical: empty host`);
  }
  if (rawHost !== rawHost.trim()) {
    throw new Error(`canonical: host has leading/trailing whitespace: ${JSON.stringify(rawHost)}`);
  }
  if (rawHost.includes("*")) {
    throw new Error(`canonical: wildcard hosts are not allowed in v1: ${rawHost}`);
  }
  for (let i = 0; i < rawHost.length; i++) {
    const code = rawHost.charCodeAt(i);
    if (code < 0x20 || code === 0x7f) {
      throw new Error(`canonical: control byte in host: ${JSON.stringify(rawHost)}`);
    }
  }
  if (rawHost.endsWith(".")) {
    throw new Error(`canonical: host has trailing dot: ${rawHost}`);
  }
  if (rawHost.startsWith(".")) {
    throw new Error(`canonical: host has leading dot: ${rawHost}`);
  }
  if (rawHost.includes("..")) {
    throw new Error(`canonical: host has consecutive dots: ${rawHost}`);
  }

  let isAscii = true;
  for (let i = 0; i < rawHost.length; i++) {
    if (rawHost.charCodeAt(i) > 0x7f) {
      isAscii = false;
      break;
    }
  }

  let candidate: string;
  if (isAscii) {
    // Strict — uppercase is rejected, not silently lowercased. The
    // canonical bytes MUST already be lowercase upstream (see S12.a
    // rule #7); accepting uppercase here would mask drift between
    // operator-authored YAML and the signed bytes.
    candidate = rawHost;
  } else {
    // IDNA-2008 to A-label via WHATWG URL parser, which uses UTS46
    // (compatible with IDNA-2008 nontransitional). The result is
    // lowercase ASCII Punycode (`xn--…`).
    let parsed: URL;
    try {
      parsed = new URL(`http://${rawHost}/`);
    } catch {
      throw new Error(`canonical: cannot IDNA-encode host: ${rawHost}`);
    }
    candidate = parsed.hostname;
  }

  if (!HOST_ASCII_RE.test(candidate)) {
    throw new Error(
      `canonical: host fails canonical regex (must be lowercase ASCII matching [a-z0-9.-]): ${rawHost}`,
    );
  }
  return candidate;
}

/**
 * Produces canonical YAML per `docs/internal/policy-canonical-format.md`.
 * Sorted, deduped, IDNA-normalized, port-explicit, block-style, LF
 * line endings with trailing newline. Byte-stable.
 */
export function buildCanonicalAllowlist(
  input: CanonicalAllowlistInput,
): CanonicalAllowlistOutput {
  if (!Number.isInteger(input.generation) || input.generation < 1) {
    throw new Error(
      `canonical: metadata.generation must be a positive integer, got ${input.generation}`,
    );
  }
  if (!Array.isArray(input.endpoints) || input.endpoints.length === 0) {
    // S12.a rule #13 reserves `[]` as a valid empty allowlist, but this
    // CLI flow always derives endpoints from the live ClawSandbox spec.
    // An empty list at this stage almost certainly indicates a bug
    // (e.g., kubectl returned nothing) — fail loud.
    throw new Error(
      `canonical: endpoints list is empty; refusing to sign an empty allowlist via --sign (use kubectl directly if intentional)`,
    );
  }

  const seen = new Set<string>();
  const normalized: CanonicalEndpoint[] = [];
  for (const ep of input.endpoints) {
    if (!ep || typeof ep !== "object") {
      throw new Error(`canonical: endpoint is not an object`);
    }
    const host = normalizeHost(ep.host);
    const port = ep.port;
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      throw new Error(
        `canonical: port out of range [1,65535]: host=${host} port=${port}`,
      );
    }
    const key = `${host}|${port}`;
    if (seen.has(key)) continue;
    seen.add(key);
    normalized.push({ host, port });
  }

  normalized.sort((a, b) => {
    if (a.host < b.host) return -1;
    if (a.host > b.host) return 1;
    return a.port - b.port;
  });

  // Hand-rolled YAML emitter — block style only, fixed key order.
  const lines: string[] = [];
  lines.push(`apiVersion: azureclaw.dev/v1alpha1`);
  lines.push(`kind: EgressAllowlist`);
  lines.push(`metadata:`);
  lines.push(`  generation: ${input.generation}`);
  lines.push(`spec:`);
  if (normalized.length === 0) {
    lines.push(`  endpoints: []`);
  } else {
    lines.push(`  endpoints:`);
    for (const ep of normalized) {
      lines.push(`    - host: ${ep.host}`);
      lines.push(`      port: ${ep.port}`);
    }
  }
  const yaml = lines.join("\n") + "\n";
  return { yaml, endpoints: normalized };
}

/** sha256:<hex> over the raw canonical bytes. */
export function digestOfCanonical(yaml: string): string {
  return "sha256:" + createHash("sha256").update(yaml, "utf8").digest("hex");
}

/**
 * Detects `oras` and `cosign` in `$PATH`. Throws with an actionable
 * error message (including install URL) if either is missing.
 */
export async function ensureSigningTools(): Promise<{ orasPath: string; cosignPath: string }> {
  const { execa } = await import("execa");
  const orasPath = await whichWith(execa, "oras");
  if (!orasPath) {
    throw new Error(
      `oras not found in $PATH. Install: https://oras.land/docs/installation`,
    );
  }
  const cosignPath = await whichWith(execa, "cosign");
  if (!cosignPath) {
    throw new Error(
      `cosign not found in $PATH. Install: https://docs.sigstore.dev/cosign/installation`,
    );
  }
  return { orasPath, cosignPath };
}

type ExecaLike = (file: string, args?: readonly string[], opts?: any) => any;

async function whichWith(execa: ExecaLike, bin: string): Promise<string | null> {
  try {
    const result = await execa("which", [bin], { stdio: "pipe" });
    const stdout = String(result.stdout ?? "").trim();
    return stdout.length > 0 ? stdout : null;
  } catch {
    return null;
  }
}

export interface PushArtifactOpts {
  orasPath: string;
  registry: string;
  repository: string;
  yaml: string;
  artifactType: string;
  /** Tag to push under (defaults to "latest"). The cosign signature is
   *  applied by digest, so the tag is informational only. */
  tag?: string;
  /** Override execa for testing. */
  execaImpl?: ExecaLike;
  /** Override workdir for testing. */
  workdir?: string;
}

/**
 * Build the argv for `oras push`. Exposed for tests so we can lock in
 * the exact invocation without spawning real processes.
 */
export function buildOrasPushArgv(opts: {
  registry: string;
  repository: string;
  artifactType: string;
  tag?: string;
  filename: string;
}): string[] {
  const tag = opts.tag ?? "latest";
  return [
    "push",
    `${opts.registry}/${opts.repository}:${tag}`,
    "--artifact-type",
    opts.artifactType,
    "--format",
    "json",
    `${opts.filename}:${opts.artifactType}`,
  ];
}

/**
 * Pushes canonical bytes as an OCI artifact via `oras` and returns the
 * resulting `sha256:…` digest. The producer-side digest is recomputed
 * locally and compared to oras's report; mismatch is a producer bug
 * (per S12.a "Signing" section) and aborts.
 */
export async function pushArtifact(opts: PushArtifactOpts): Promise<string> {
  const { execa } = await import("execa");
  const exec: ExecaLike = opts.execaImpl ?? execa;
  const expected = digestOfCanonical(opts.yaml);

  const dir = opts.workdir ?? mkdtempSync(join(tmpdir(), "azureclaw-egress-"));
  const filename = "allowlist.yaml";
  const fullPath = join(dir, filename);
  writeFileSync(fullPath, opts.yaml, { encoding: "utf8" });
  try {
    const argv = buildOrasPushArgv({
      registry: opts.registry,
      repository: opts.repository,
      artifactType: opts.artifactType,
      tag: opts.tag,
      filename,
    });
    const result = await exec(opts.orasPath, argv, { cwd: dir, stdio: "pipe" });
    const stdout = String(result.stdout ?? "");
    const reported = parseOrasDigest(stdout);
    if (!reported) {
      throw new Error(`oras push: could not parse digest from output: ${stdout.slice(0, 200)}`);
    }
    if (reported !== expected) {
      throw new Error(
        `oras push reported digest ${reported}; producer computed ${expected}. Canonical bytes diverged — aborting before signing.`,
      );
    }
    return reported;
  } finally {
    if (!opts.workdir) {
      try { rmSync(dir, { recursive: true, force: true }); } catch { /* ignore */ }
    }
  }
}

/** Parse the `digest` from `oras push --format json` output. */
export function parseOrasDigest(stdout: string): string | null {
  const trimmed = stdout.trim();
  if (trimmed.length === 0) return null;
  try {
    const obj = JSON.parse(trimmed);
    const candidate =
      obj?.reference?.digest ??
      obj?.manifest?.digest ??
      obj?.digest ??
      null;
    if (typeof candidate === "string" && /^sha(256|384|512):[a-f0-9]+$/.test(candidate)) {
      return candidate;
    }
  } catch {
    // fall through to text-mode parse below
  }
  // Text fallback: oras text output contains "Digest: sha256:…".
  const match = trimmed.match(/Digest:\s*(sha(?:256|384|512):[a-f0-9]+)/);
  if (match) return match[1];
  return null;
}

export interface SignArtifactOpts {
  cosignPath: string;
  registry: string;
  repository: string;
  digest: string;
  mode: SignMode;
  keyRef?: string;
  identityToken?: string;
  execaImpl?: ExecaLike;
}

/**
 * Build the argv for `cosign sign` for the given mode. Exposed for
 * tests so we can lock in the exact invocation.
 */
export function buildCosignSignArgv(opts: {
  registry: string;
  repository: string;
  digest: string;
  mode: SignMode;
  keyRef?: string;
  identityToken?: string;
}): string[] {
  const target = `${opts.registry}/${opts.repository}@${opts.digest}`;
  const argv: string[] = ["sign", "--yes"];
  switch (opts.mode) {
    case "keyless":
      // No auth flag — cosign launches the OIDC browser flow.
      break;
    case "identity-token": {
      const tok = opts.identityToken ?? process.env.SIGSTORE_ID_TOKEN ?? process.env.OIDC_TOKEN;
      if (!tok) {
        throw new Error(`cosign identity-token mode requires SIGSTORE_ID_TOKEN or OIDC_TOKEN env`);
      }
      argv.push("--identity-token", tok);
      break;
    }
    case "keyed":
      if (!opts.keyRef || opts.keyRef.length === 0) {
        throw new Error(`cosign keyed mode requires --sign-key`);
      }
      argv.push("--key", opts.keyRef);
      break;
  }
  argv.push(target);
  return argv;
}

export async function signArtifact(opts: SignArtifactOpts): Promise<void> {
  const { execa } = await import("execa");
  const exec: ExecaLike = opts.execaImpl ?? execa;
  const argv = buildCosignSignArgv(opts);
  await exec(opts.cosignPath, argv, { stdio: "inherit" });
}

export function autoDetectSignMode(opts: {
  signModeFlag?: string;
  signKey?: string;
  isTTY: boolean;
  env: NodeJS.ProcessEnv;
}): SignMode {
  const flag = opts.signModeFlag;
  if (flag) {
    if (flag !== "keyless" && flag !== "identity-token" && flag !== "keyed") {
      throw new Error(
        `--sign-mode must be one of: keyless, identity-token, keyed (got: ${flag})`,
      );
    }
    if (flag === "keyed" && (!opts.signKey || opts.signKey.length === 0)) {
      throw new Error(`--sign-mode keyed requires --sign-key`);
    }
    return flag;
  }
  // No explicit mode — auto-detect.
  if (opts.env.SIGSTORE_ID_TOKEN || opts.env.OIDC_TOKEN) {
    return "identity-token";
  }
  if (opts.signKey && opts.signKey.length > 0) {
    return "keyed";
  }
  if (opts.isTTY) {
    return "keyless";
  }
  // Non-TTY without token and without key → cannot proceed.
  throw new Error(
    `--sign requires one of: TTY for keyless OIDC, SIGSTORE_ID_TOKEN/OIDC_TOKEN env, or --sign-mode keyed --sign-key <ref>`,
  );
}

export interface PatchClawSandboxOpts {
  kubectlPath: string;
  namespace: string;
  name: string;
  registry: string;
  repository: string;
  digest: string;
  artifactType: string;
  execaImpl?: ExecaLike;
}

/**
 * Build the kubectl argv for the merge patch. Exposed for tests.
 */
export function buildPatchArgv(opts: Omit<PatchClawSandboxOpts, "kubectlPath" | "execaImpl">): string[] {
  const patch = {
    spec: {
      networkPolicy: {
        allowlistRef: {
          registry: opts.registry,
          repository: opts.repository,
          digest: opts.digest,
          artifactType: opts.artifactType,
        },
      },
    },
  };
  return [
    "patch",
    `clawsandbox/${opts.name}`,
    "-n",
    opts.namespace,
    "--type=merge",
    "-p",
    JSON.stringify(patch),
  ];
}

export async function patchClawSandbox(opts: PatchClawSandboxOpts): Promise<void> {
  const { execa } = await import("execa");
  const exec: ExecaLike = opts.execaImpl ?? execa;
  const argv = buildPatchArgv(opts);
  await exec(opts.kubectlPath, argv, { stdio: "pipe" });
}

/**
 * Read live `allowedEndpoints` and `metadata.generation` from a
 * ClawSandbox via kubectl. Used to feed the canonical builder.
 */
export async function readClawSandboxState(opts: {
  kubectlPath: string;
  namespace: string;
  name: string;
  execaImpl?: ExecaLike;
}): Promise<{ generation: number; endpoints: Array<{ host: string; port: number }> }> {
  const { execa } = await import("execa");
  const exec: ExecaLike = opts.execaImpl ?? execa;
  const result = await exec(
    opts.kubectlPath,
    ["get", `clawsandbox/${opts.name}`, "-n", opts.namespace, "-o", "json"],
    { stdio: "pipe" },
  );
  const obj = JSON.parse(String(result.stdout ?? "{}"));
  const generation = Number(obj?.metadata?.generation);
  if (!Number.isInteger(generation) || generation < 1) {
    throw new Error(
      `kubectl: ClawSandbox ${opts.namespace}/${opts.name} has no positive metadata.generation`,
    );
  }
  const raw: unknown = obj?.spec?.networkPolicy?.allowedEndpoints;
  const endpoints: Array<{ host: string; port: number }> = [];
  if (Array.isArray(raw)) {
    for (const ep of raw) {
      const e = ep as { host?: unknown; port?: unknown };
      if (typeof e.host === "string" && typeof e.port === "number") {
        endpoints.push({ host: e.host, port: e.port });
      }
    }
  }
  return { generation, endpoints };
}

// ---- S12.g — `--emit-manifest` GitOps mode ---------------------------------

export interface EmitManifestInput {
  namespace: string;
  name: string;
  registry: string;
  repository: string;
  digest: string;
  artifactType: string;
  /** Cosign signer identity (e.g. Fulcio SAN/issuer pair, or KMS key
   *  ref). Surfaced in the file's leading comment for human review. */
  signerIdentity: string;
}

/**
 * Build the byte-stable strategic-merge patch YAML for a `ClawSandbox`
 * with `spec.networkPolicy.allowlistRef` set, plus the marker
 * annotation `azureclaw.io/applied-via-gitops=true`. The output is a
 * complete, valid resource (apiVersion + kind + metadata + spec only)
 * suitable for `kubectl apply -f` from a GitOps controller.
 *
 * Determinism guarantees (so the file diff cleanly under git review):
 *   - Fixed key order, hand-rolled emitter (no `yaml`/`js-yaml`
 *     defaults that may reorder keys across versions).
 *   - LF line endings, single trailing newline, no trailing whitespace.
 *   - No timestamps, no random IDs, no source-host metadata.
 *
 * The first line is a `# ` comment containing the artifact digest +
 * signer identity for human review during PR review.
 */
export function buildEmitManifestYaml(input: EmitManifestInput): string {
  if (!input.namespace || !input.name) {
    throw new Error("emit-manifest: namespace and name are required");
  }
  if (!/^sha(256|384|512):[a-f0-9]+$/.test(input.digest)) {
    throw new Error(`emit-manifest: malformed digest: ${input.digest}`);
  }
  if (!input.signerIdentity || input.signerIdentity.length === 0) {
    throw new Error("emit-manifest: signerIdentity is required");
  }
  const lines: string[] = [];
  lines.push(
    `# azureclaw egress allowlist — digest=${input.digest} signer=${input.signerIdentity}`,
  );
  lines.push(`# Generated by 'azureclaw egress … --emit-manifest'.`);
  lines.push(`# Commit this file unchanged; your GitOps controller applies it.`);
  lines.push(`apiVersion: azureclaw.azure.com/v1alpha1`);
  lines.push(`kind: ClawSandbox`);
  lines.push(`metadata:`);
  lines.push(`  name: ${input.name}`);
  lines.push(`  namespace: ${input.namespace}`);
  lines.push(`  annotations:`);
  lines.push(`    azureclaw.io/applied-via-gitops: "true"`);
  lines.push(`spec:`);
  lines.push(`  networkPolicy:`);
  lines.push(`    allowlistRef:`);
  lines.push(`      registry: ${input.registry}`);
  lines.push(`      repository: ${input.repository}`);
  lines.push(`      digest: ${input.digest}`);
  lines.push(`      artifactType: ${input.artifactType}`);
  return lines.join("\n") + "\n";
}

export interface WriteEmitManifestOpts {
  path: string;
  yaml: string;
  force: boolean;
  /** Test override — defaults to `node:fs` `existsSync`/`writeFileSync`. */
  fsImpl?: {
    existsSync: (p: string) => boolean;
    writeFileSync: (p: string, data: string, opts: { encoding: "utf8" }) => void;
  };
}

/**
 * Write the emit-manifest YAML to disk. Refuses to overwrite an
 * existing file unless `force` is set — this prevents accidental
 * clobbering in CI pipelines that re-run the same step.
 */
export function writeEmitManifest(opts: WriteEmitManifestOpts): void {
  const fs = opts.fsImpl ?? { existsSync, writeFileSync };
  if (fs.existsSync(opts.path) && !opts.force) {
    throw new Error(
      `emit-manifest: refusing to overwrite existing file ${opts.path} (pass --force to override)`,
    );
  }
  fs.writeFileSync(opts.path, opts.yaml, { encoding: "utf8" });
}

/**
 * Resolve the human-readable cosign signer identity for the manifest
 * comment header. Returns:
 *   - `keyless:<issuer>:<subject>` when keyless mode is in effect (we
 *     defer to cosign for the actual identity binding; this is purely
 *     informational text for git review),
 *   - `identity-token:<issuer-or-anonymous>` for OIDC token mode,
 *   - `keyed:<keyRef>` for the keyed mode,
 *   - `unknown` as a last resort (never throws — a missing comment
 *     header would be worse than an imperfect one).
 */
export function describeSignerIdentity(opts: {
  mode: SignMode;
  keyRef?: string;
  identityToken?: string;
  env?: NodeJS.ProcessEnv;
}): string {
  switch (opts.mode) {
    case "keyless":
      return "keyless:fulcio";
    case "identity-token": {
      const env = opts.env ?? process.env;
      const tok = opts.identityToken ?? env.SIGSTORE_ID_TOKEN ?? env.OIDC_TOKEN;
      // Best-effort issuer hint — we do NOT decode the JWT here (no
      // crypto verify, no PII leak); we just call out the env source.
      const src = env.SIGSTORE_ID_TOKEN
        ? "SIGSTORE_ID_TOKEN"
        : env.OIDC_TOKEN
          ? "OIDC_TOKEN"
          : tok
            ? "explicit"
            : "unset";
      return `identity-token:${src}`;
    }
    case "keyed":
      return `keyed:${opts.keyRef ?? "unknown"}`;
    default:
      return "unknown";
  }
}
