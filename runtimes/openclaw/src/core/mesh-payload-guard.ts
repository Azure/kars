// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Mesh-send payload guard.
//
// Demo-day failure mode: the LLM constructs a `mesh_send` content that
// either (a) advertises a `file_transfer` with placeholder text where
// real base64 should be (`<base64-image-data>`, `<base64-bytes>`,
// `<base64>`), (b) advertises a `file_transfer` with no `file_data` at
// all, or (c) sends a JSON object whose only file reference is a local
// container path the peer cannot resolve (`/sandbox/...`, `/tmp/...`)
// without inlining the bytes.
//
// Recipient agents run in their own containers — they cannot read
// `/sandbox/<peer>/...` no matter what the path string says. The right
// path is the `mesh_transfer_file` tool (sub-agent) or
// `azureclaw_mesh_transfer_file` (parent), which read-and-base64-encode
// the file and send a proper `file_transfer` envelope.
//
// `validateMeshPayload(content, hint)` returns `null` when the payload
// looks safe to send and an error string (with the hint to point at
// the right tool) otherwise. The caller should surface the error
// string back to the LLM verbatim so the next round corrects course.

const PLACEHOLDERS = new Set([
  "<base64>",
  "<base64-data>",
  "<base64-bytes>",
  "<base64-image-data>",
  "<base64-bytes-here>",
  "<file_data>",
  "<your-base64-here>",
]);

const LOCAL_PATH_KEYS = new Set([
  "file_path",
  "filepath",
  "artifact_path",
  "hero_image_path",
  "image_path",
  "chart_path",
  "path",
]);

const LOCAL_PATH_PREFIXES = ["/sandbox/", "/tmp/", "/mnt/data/", "/workspace/"];

function looksLikeLocalPath(value: unknown): boolean {
  if (typeof value !== "string") return false;
  return LOCAL_PATH_PREFIXES.some((p) => value.startsWith(p));
}

function isPlaceholder(value: unknown): boolean {
  if (typeof value !== "string") return false;
  const lower = value.trim().toLowerCase();
  if (PLACEHOLDERS.has(lower)) return true;
  // Generic catch — angle-bracketed token where real base64 cannot have <>.
  return /^<[a-z0-9_\- ]+>$/i.test(lower);
}

function decodesAsBase64(value: string): boolean {
  if (!value) return false;
  // Reject angle brackets — base64 alphabet is A-Za-z0-9+/=.
  if (/[<>]/.test(value)) return false;
  try {
    const buf = Buffer.from(value, "base64");
    return buf.length > 0;
  } catch {
    return false;
  }
}

export interface MeshGuardOptions {
  /** Tool name to suggest in the error message (varies parent vs sub-agent). */
  transferToolName: string;
}

/**
 * Validate a mesh_send payload string.
 *
 * Returns `null` when the payload looks safe to send across containers,
 * or an error string (with remediation hint) otherwise.
 *
 * @param content - the mesh_send `content` / `message` string the LLM produced.
 * @param opts.transferToolName - which file-transfer tool to point at.
 */
export function validateMeshPayload(
  content: unknown,
  opts: MeshGuardOptions,
): string | null {
  if (typeof content !== "string" || content.length === 0) {
    return null; // empty / non-string handled elsewhere
  }

  // Cheap early exit — if it doesn't look like JSON, it's free-form text
  // (status / prose) which we explicitly do NOT want to false-reject.
  const trimmed = content.trim();
  if (!trimmed.startsWith("{") && !trimmed.startsWith("[")) {
    return null;
  }

  let parsed: any;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return null; // malformed JSON — let the receiver deal with it
  }

  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return null;
  }

  // ── 1. Explicit file_transfer envelope ────────────────────────────
  if (parsed.type === "file_transfer") {
    const fileData = parsed.file_data;

    if (fileData == null || (typeof fileData === "string" && fileData.length === 0)) {
      return (
        "REJECTED: file_transfer envelope is missing file_data. " +
        "Peer agents run in separate containers — they cannot read " +
        `your local /sandbox or /tmp paths. Use the \`${opts.transferToolName}\` ` +
        "tool to send a real file (it will read and base64-encode the bytes for you), " +
        "or inline the actual base64 in `file_data` if you produced the bytes in-memory."
      );
    }

    if (typeof fileData !== "string") {
      return (
        "REJECTED: file_transfer.file_data must be a base64 string. " +
        `Use the \`${opts.transferToolName}\` tool instead of constructing this envelope by hand.`
      );
    }

    if (isPlaceholder(fileData)) {
      return (
        "REJECTED: file_transfer.file_data contains a placeholder " +
        `(\`${fileData}\`) instead of real base64 bytes. The recipient cannot ` +
        `decode this. Use the \`${opts.transferToolName}\` tool to send the actual ` +
        "file — it reads the bytes and base64-encodes them for you."
      );
    }

    if (!decodesAsBase64(fileData)) {
      return (
        "REJECTED: file_transfer.file_data is not valid base64. " +
        `Use the \`${opts.transferToolName}\` tool instead of constructing the envelope by hand.`
      );
    }

    // size_bytes sanity check (best effort — non-fatal mismatch is OK).
    return null;
  }

  // ── 2. Plain JSON metadata referencing a local container path ────
  // Only reject when the value clearly points into the sender's
  // container filesystem AND there is no inlined data the peer could
  // use instead. Plain JSON like {"summary":"see /sandbox/xyz"} that
  // doesn't use a known *_path key is accepted (user prose).
  for (const [k, v] of Object.entries(parsed)) {
    if (LOCAL_PATH_KEYS.has(k) && looksLikeLocalPath(v)) {
      const hasInline =
        typeof parsed.file_data === "string" && parsed.file_data.length > 0;
      const hasContent =
        typeof parsed.content === "string" && parsed.content.length > 0;
      if (!hasInline && !hasContent) {
        return (
          `REJECTED: payload references a local container path (\`${k}: ${v}\`) ` +
          "but contains no inlined data. Peer agents cannot read your filesystem. " +
          `Use the \`${opts.transferToolName}\` tool to send the file, or inline ` +
          "the bytes/text directly in the message."
        );
      }
    }
  }

  return null;
}
