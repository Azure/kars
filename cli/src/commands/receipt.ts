// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Inc 3 — `kars receipt` CLI subcommand.
//
// The Governance Receipt is a signed, independently-verifiable record that a
// `KarsTask` was governed under a specific trust envelope. This command is the
// **auditor's tool**: it verifies the cryptographic signature against an
// out-of-band trust anchor — it does not trust the Bridge UI, the receipt's
// own embedded fields, or anything but the controller's published public key.
//
// `kars receipt verify <task>`:
//   1. Reads the `KarsReceipt` CR for the task.
//   2. Reads the trust anchor (`kars-receipt-pubkey` ConfigMap in
//      `kars-system`) — the controller's public key, published out of band.
//   3. Reconstructs the DSSE Pre-Authentication Encoding over the signed
//      in-toto Statement and verifies the Ed25519 signature.
//   4. Cross-checks that the signed subject digest matches the receipt's
//      claimed `envelopeDigest`, and that the signing `keyid` matches the
//      anchor — defeating a forged receipt that swaps in its own key.
//   5. Prints the claim matrix (integrity / conformance / completeness /
//      regulatory) verbatim and an overall verdict. Exits non-zero on any
//      signature, anchor, or binding failure.
//
// This works on a **plain kars cluster with no Bridge installed** — the
// receipt is a kars primitive.

import { Command } from "commander";
import chalk from "chalk";
import { createPublicKey, verify as cryptoVerify } from "node:crypto";

const ANCHOR_NAMESPACE = "kars-system";
const ANCHOR_CONFIGMAP = "kars-receipt-pubkey";
const DSSE_PAYLOAD_TYPE = "application/vnd.in-toto+json";
// Fixed ASN.1/DER SubjectPublicKeyInfo prefix for an Ed25519 public key
// (RFC 8410). Prepending it to the 32 raw key bytes yields a SPKI DER that
// Node's crypto can import.
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

interface DsseSignature {
  keyid: string;
  sig: string;
}

interface DsseEnvelope {
  payload: string;
  payloadType: string;
  signatures: DsseSignature[];
}

interface Claim {
  class: string;
  status: string;
  detail: string;
}

interface ReceiptSpec {
  taskRef?: { name?: string };
  envelopeDigest?: string;
  predicateType?: string;
  scheme?: string;
  keyId?: string;
  dsse?: DsseEnvelope;
  claims?: Claim[];
}

interface ReceiptCr {
  metadata?: { name?: string; namespace?: string; creationTimestamp?: string };
  spec?: ReceiptSpec;
  status?: { issuedAt?: string; observedTaskGeneration?: number };
}

interface TrustAnchor {
  keyId: string;
  publicKey: string; // base64, 32 raw Ed25519 bytes
  scheme: string;
  payloadType: string;
}

export interface VerifyResult {
  ok: boolean;
  task: string;
  namespace: string;
  keyId: string;
  envelopeDigest: string | null;
  /** Per-check pass/fail with human reasons. */
  checks: Array<{ name: string; ok: boolean; detail: string }>;
  claims: Claim[];
  /** The decoded in-toto Statement, for `--format json` consumers. */
  statement: unknown;
}

/**
 * DSSE Pre-Authentication Encoding, byte-identical to the Rust emitter:
 * `"DSSEv1 " len(type) " " type " " len(body) " " body`. Lengths are byte
 * lengths.
 */
export function pae(payloadType: string, body: Buffer): Buffer {
  const typeBytes = Buffer.from(payloadType, "utf8");
  return Buffer.concat([
    Buffer.from("DSSEv1 ", "utf8"),
    Buffer.from(String(typeBytes.length), "utf8"),
    Buffer.from(" ", "utf8"),
    typeBytes,
    Buffer.from(" ", "utf8"),
    Buffer.from(String(body.length), "utf8"),
    Buffer.from(" ", "utf8"),
    body,
  ]);
}

/** Import 32 raw Ed25519 public-key bytes as a verifiable KeyObject. */
function importEd25519PublicKey(raw: Buffer) {
  const der = Buffer.concat([ED25519_SPKI_PREFIX, raw]);
  return createPublicKey({ key: der, format: "der", type: "spki" });
}

/**
 * Verify a receipt against a trust anchor. Pure (no I/O) so it is unit
 * testable; the command wires it to `kubectl`.
 */
export function verifyReceipt(receipt: ReceiptCr, anchor: TrustAnchor): VerifyResult {
  const spec = receipt.spec ?? {};
  const task = receipt.metadata?.name ?? spec.taskRef?.name ?? "(unknown)";
  const namespace = receipt.metadata?.namespace ?? "(unknown)";
  const checks: VerifyResult["checks"] = [];

  const dsse = spec.dsse;
  let statement: unknown = null;
  let payloadBody: Buffer | null = null;

  if (!dsse || !Array.isArray(dsse.signatures) || dsse.signatures.length === 0) {
    checks.push({ name: "envelope", ok: false, detail: "receipt has no DSSE envelope or signatures" });
  } else {
    payloadBody = Buffer.from(dsse.payload ?? "", "base64");
    try {
      statement = JSON.parse(payloadBody.toString("utf8"));
      checks.push({ name: "payload", ok: true, detail: "in-toto Statement decoded" });
    } catch {
      checks.push({ name: "payload", ok: false, detail: "DSSE payload is not valid JSON" });
    }

    // Payload type must match what we sign over.
    const ptOk = dsse.payloadType === DSSE_PAYLOAD_TYPE;
    checks.push({
      name: "payloadType",
      ok: ptOk,
      detail: ptOk ? DSSE_PAYLOAD_TYPE : `unexpected payloadType '${dsse.payloadType}'`,
    });

    // Key binding: the signature keyid and the anchor must agree, and match
    // the receipt's declared keyId. This is what stops a forged receipt from
    // shipping its own key.
    const sig = dsse.signatures[0];
    const keyMatchesAnchor = sig.keyid === anchor.keyId;
    const declaredMatches = !spec.keyId || spec.keyId === anchor.keyId;
    checks.push({
      name: "keyBinding",
      ok: keyMatchesAnchor && declaredMatches,
      detail:
        keyMatchesAnchor && declaredMatches
          ? `signed by trusted anchor ${anchor.keyId.slice(0, 16)}…`
          : `keyid mismatch (sig=${sig.keyid.slice(0, 16)}… anchor=${anchor.keyId.slice(0, 16)}…)`,
    });

    // The cryptographic core: verify Ed25519 over the PAE.
    if (payloadBody) {
      let sigOk = false;
      let sigDetail = "";
      try {
        const raw = Buffer.from(anchor.publicKey, "base64");
        const key = importEd25519PublicKey(raw);
        const message = pae(DSSE_PAYLOAD_TYPE, payloadBody);
        const signature = Buffer.from(sig.sig ?? "", "base64");
        sigOk = cryptoVerify(null, message, key, signature);
        sigDetail = sigOk
          ? "DSSE/Ed25519 signature valid"
          : "DSSE/Ed25519 signature INVALID";
      } catch (e) {
        sigDetail = `signature verification error: ${(e as Error).message}`;
      }
      checks.push({ name: "signature", ok: sigOk, detail: sigDetail });
    }
  }

  // Binding: the signed subject digest must match the receipt's claimed
  // envelopeDigest (sans the `sha256:` prefix the in-toto field drops).
  const claimedDigest = spec.envelopeDigest ?? null;
  if (statement && claimedDigest) {
    const subj = (statement as { subject?: Array<{ digest?: { sha256?: string } }> }).subject;
    const signedDigest = subj?.[0]?.digest?.sha256 ?? null;
    const want = claimedDigest.replace(/^sha256:/, "");
    const bound = signedDigest === want;
    checks.push({
      name: "envelopeBinding",
      ok: bound,
      detail: bound
        ? `subject bound to envelope ${claimedDigest}`
        : `subject digest '${signedDigest}' != claimed '${want}'`,
    });
  }

  const ok = checks.length > 0 && checks.every((c) => c.ok);
  return {
    ok,
    task,
    namespace,
    keyId: anchor.keyId,
    envelopeDigest: claimedDigest,
    checks,
    claims: spec.claims ?? [],
    statement,
  };
}

async function kubectlGetJson(args: string[]): Promise<unknown | null> {
  const { execa } = await import("execa");
  try {
    const { stdout } = await execa("kubectl", [...args, "-o", "json"], { stdio: "pipe" });
    return JSON.parse(stdout);
  } catch {
    return null;
  }
}

async function fetchAnchor(): Promise<TrustAnchor | null> {
  const cm = (await kubectlGetJson([
    "get",
    "configmap",
    ANCHOR_CONFIGMAP,
    "-n",
    ANCHOR_NAMESPACE,
  ])) as { data?: Record<string, string> } | null;
  const data = cm?.data;
  if (!data?.keyId || !data?.publicKey) return null;
  return {
    keyId: data.keyId,
    publicKey: data.publicKey,
    scheme: data.scheme ?? "DSSEv1+ed25519",
    payloadType: data.payloadType ?? DSSE_PAYLOAD_TYPE,
  };
}

function statusBadge(status: string): string {
  switch (status) {
    case "PASS":
      return chalk.green("PASS");
    case "PARTIAL":
      return chalk.yellow("PARTIAL");
    case "OMITTED":
      return chalk.gray("OMITTED");
    case "FAIL":
      return chalk.red("FAIL");
    default:
      return status;
  }
}

function formatHuman(result: VerifyResult): string {
  const lines: string[] = [];
  const verdict = result.ok
    ? chalk.green.bold("✓ VERIFIED")
    : chalk.red.bold("✗ NOT VERIFIED");
  lines.push("");
  lines.push(`  ${chalk.bold("Governance Receipt")}  ${result.namespace}/${result.task}`);
  lines.push(`  ${chalk.bold("Verdict:")}            ${verdict}`);
  if (result.envelopeDigest) {
    lines.push(`  ${chalk.bold("Envelope:")}           ${result.envelopeDigest}`);
  }
  lines.push(`  ${chalk.bold("Signed by:")}          ${result.keyId.slice(0, 24)}…`);
  lines.push("");
  lines.push(`  ${chalk.bold.underline("Cryptographic checks")}`);
  for (const c of result.checks) {
    const mark = c.ok ? chalk.green("✓") : chalk.red("✗");
    lines.push(`    ${mark} ${c.name.padEnd(16)} ${chalk.dim(c.detail)}`);
  }
  lines.push("");
  lines.push(`  ${chalk.bold.underline("Claim matrix")}`);
  for (const claim of result.claims) {
    lines.push(`    ${statusBadge(claim.status).padEnd(18)} ${chalk.bold(claim.class)}`);
    lines.push(`        ${chalk.dim(claim.detail)}`);
  }
  lines.push("");
  return lines.join("\n");
}

export function receiptCommand(): Command {
  const cmd = new Command("receipt");
  cmd.description(
    "Inspect and verify Governance Receipts — signed, independently-" +
      "verifiable records that a KarsTask was governed under a trust envelope.",
  );

  cmd
    .command("verify")
    .description(
      "Cryptographically verify a task's Governance Receipt against the " +
        "controller's published trust anchor. Exits non-zero if the signature, " +
        "key binding, or envelope binding fails.",
    )
    .argument("<task>", "KarsTask name")
    .option("-n, --namespace <ns>", "Namespace where the KarsReceipt lives", "kars-system")
    .option("--format <fmt>", "Output format: 'human' (default) or 'json'", "human")
    .action(async (task: string, options: { namespace: string; format: string }) => {
      const receipt = (await kubectlGetJson([
        "get",
        "karsreceipt",
        task,
        "-n",
        options.namespace,
      ])) as ReceiptCr | null;
      if (!receipt) {
        process.stderr.write(
          chalk.red(
            `✗ no Governance Receipt found for '${task}' in namespace '${options.namespace}'.\n` +
              `  A receipt is emitted only for a governance-Ready task.\n`,
          ),
        );
        process.exit(4);
        return;
      }

      const anchor = await fetchAnchor();
      if (!anchor) {
        process.stderr.write(
          chalk.red(
            `✗ trust anchor '${ANCHOR_CONFIGMAP}' not found in '${ANCHOR_NAMESPACE}'.\n` +
              `  Cannot verify a receipt without the controller's published public key.\n`,
          ),
        );
        process.exit(5);
        return;
      }

      const result = verifyReceipt(receipt, anchor);
      if (options.format === "json") {
        console.log(JSON.stringify(result, null, 2));
      } else {
        console.log(formatHuman(result));
      }
      if (!result.ok) {
        process.exit(2);
      }
    });

  cmd
    .command("show")
    .description("Print the raw Governance Receipt (DSSE envelope + claims) for a task.")
    .argument("<task>", "KarsTask name")
    .option("-n, --namespace <ns>", "Namespace where the KarsReceipt lives", "kars-system")
    .action(async (task: string, options: { namespace: string }) => {
      const receipt = (await kubectlGetJson([
        "get",
        "karsreceipt",
        task,
        "-n",
        options.namespace,
      ])) as ReceiptCr | null;
      if (!receipt) {
        process.stderr.write(
          chalk.red(`✗ no Governance Receipt found for '${task}' in '${options.namespace}'.\n`),
        );
        process.exit(4);
        return;
      }
      console.log(JSON.stringify(receipt, null, 2));
    });

  return cmd;
}

export const __test = { pae, verifyReceipt, importEd25519PublicKey };
