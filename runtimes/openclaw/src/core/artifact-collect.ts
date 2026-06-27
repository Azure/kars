// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Harness-neutral workspace artifact collection for the mesh `task_request`
// path. When the controller delivers a governed task over the mesh, the agent
// runs its native loop and may write a *set* of artifact files into its
// workspace (a research report, a data file, a decision matrix, …). This module
// harvests those files and ships them back to the requester as `file_transfer`
// frames — the same wire shape the offload path already uses — so the requester
// can assemble the complete deliverable set, not just the final text reply.
//
// The collection logic mirrors `core/agt-offload.ts` (proven in the offload
// flow). It is factored here so the mesh task path can reuse it additively
// without modifying the offload runner.

const WORKSPACE_ROOT = "/sandbox/.openclaw/workspace";

/// Make a string safe for the AGT SDK's plaintext mesh send, which encodes
/// payloads with `btoa(JSON.stringify(...))`. `btoa` throws "Invalid character"
/// on any code point > 0xFF, so LLM output containing em-dashes, smart quotes,
/// arrows, … breaks the send. We transliterate the common typographic
/// offenders to ASCII and replace any remaining >0xFF code point with '?'. This
/// only touches the short chat summary on the mesh wire — artifact file bytes
/// travel base64-encoded and keep their full Unicode intact.
export function latin1Safe(input: string): string {
  const map: Record<string, string> = {
    "\u2014": "-", "\u2013": "-", "\u2012": "-", "\u2015": "-",
    "\u2018": "'", "\u2019": "'", "\u201A": "'", "\u201B": "'",
    "\u201C": "\"", "\u201D": "\"", "\u201E": "\"", "\u2033": "\"",
    "\u2026": "...", "\u2022": "*", "\u00B7": "*", "\u2192": "->",
    "\u2190": "<-", "\u2194": "<->", "\u00D7": "x", "\u2260": "!=",
    "\u2264": "<=", "\u2265": ">=", "\u00A0": " ", "\u200B": "",
    "\u2009": " ", "\u202F": " ", "\uFE0F": "",
  };
  let out = "";
  for (const ch of input) {
    if (ch in map) {
      out += map[ch];
    } else if (ch.codePointAt(0)! > 0xff) {
      out += "?";
    } else {
      out += ch;
    }
  }
  return out;
}


// Scaffold files that always exist in a fresh workspace — never shipped as
// task artifacts.
const SCAFFOLD_FILES = new Set([
  "USER.md",
  "SOUL.md",
  "AGENTS.md",
  "TOOLS.md",
  "MEMORY.md",
  "HEARTBEAT.md",
  "IDENTITY.md",
  "workspace-state.json",
]);

export interface ArtifactManifestEntry {
  name: string;
  path: string;
  size_bytes: number;
}

interface Logger {
  info: (m: string) => void;
  warn: (m: string) => void;
}

interface ShipDeps {
  meshClient: { send: (toAmid: string, payload: unknown) => Promise<unknown> };
  toAmid: string;
  fromAgent: string;
}

/// Create a timestamp marker so `find -newer` only harvests files the task
/// actually produced (not pre-existing workspace content). Returns the marker
/// path, or "" on failure (collection then falls back to all matching files).
export async function createHarvestMarker(): Promise<string> {
  try {
    const fs = await import("node:fs");
    const os = await import("node:os");
    const path = await import("node:path");
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "task-artifacts-"));
    const marker = path.join(tmpDir, "start");
    fs.writeFileSync(marker, "", { mode: 0o600 });
    return marker;
  } catch {
    return "";
  }
}

/// Harvest new workspace artifacts produced since `harvestMarker`, ship each to
/// the requester via `file_transfer`, and return the manifest. If the task
/// produced no explicit files but returned a substantial text result, that
/// result is saved as a markdown fallback so the requester always gets at least
/// one durable artifact. Returns the manifest of shipped artifacts.
export async function collectAndShipArtifacts(
  deps: ShipDeps,
  harvestMarker: string,
  taskResult: string,
  taskSuccess: boolean,
  requestId: string,
  log: Logger,
): Promise<ArtifactManifestEntry[]> {
  const relPaths = await harvestArtifactPaths(harvestMarker, log);

  // Fallback: a substantial textual response with no explicit file → persist it
  // as a markdown artifact so the deliverable set is never empty.
  if (taskSuccess && relPaths.length === 0 && taskResult && taskResult.length > 400) {
    try {
      const fs = await import("node:fs");
      const fallbackName = `task-${requestId.slice(0, 8)}-report.md`;
      fs.mkdirSync(WORKSPACE_ROOT, { recursive: true });
      fs.writeFileSync(`${WORKSPACE_ROOT}/${fallbackName}`, taskResult, "utf-8");
      relPaths.push(fallbackName);
      log.info(`No explicit artifacts — saved task response as ${fallbackName} (${taskResult.length} chars)`);
    } catch (e) {
      log.warn(`Failed to write fallback artifact: ${(e as Error).message}`);
    }
  }

  // Clean up the harvest marker dir.
  try {
    if (harvestMarker) {
      const fs = await import("node:fs");
      const path = await import("node:path");
      fs.unlinkSync(harvestMarker);
      try {
        fs.rmdirSync(path.dirname(harvestMarker));
      } catch {
        /* ignore */
      }
    }
  } catch {
    /* ignore */
  }

  const manifest: ArtifactManifestEntry[] = [];
  for (const relPath of relPaths.slice(0, 12)) {
    try {
      const fs = await import("node:fs");
      const fPath = `${WORKSPACE_ROOT}/${relPath}`;
      // Open once to avoid a stat→read TOCTOU race (CWE-367).
      const fd = fs.openSync(fPath, "r");
      let stat: import("node:fs").Stats;
      let data: Buffer;
      try {
        stat = fs.fstatSync(fd);
        if (stat.size > 30 * 1024 * 1024) {
          fs.closeSync(fd);
          continue;
        }
        data = Buffer.alloc(stat.size);
        fs.readSync(fd, data, 0, stat.size, 0);
      } finally {
        fs.closeSync(fd);
      }
      const name = relPath.split("/").pop() || relPath;
      await deps.meshClient.send(deps.toAmid, {
        type: "file_transfer",
        file_name: name,
        file_path: relPath,
        file_data: data.toString("base64"),
        size_bytes: stat.size,
        description: `Artifact from mesh task ${requestId.slice(0, 8)}`,
        from_agent: deps.fromAgent,
        timestamp: new Date().toISOString(),
      });
      manifest.push({ name, path: relPath, size_bytes: stat.size });
      log.info(`Shipped artifact '${name}' (${(stat.size / 1024).toFixed(1)} KB) to requester`);
    } catch (e) {
      log.warn(`Failed to ship artifact '${relPath}': ${(e as Error).message}`);
    }
  }
  return manifest;
}

/// List new artifact files under the workspace (text + common binary doc types),
/// excluding scaffold and dotfiles. Mirrors the offload runner's `find` harvest.
async function harvestArtifactPaths(harvestMarker: string, log: Logger): Promise<string[]> {
  const out: string[] = [];
  try {
    const { execFileSync } = await import("node:child_process");
    // execFileSync with an arg array (no shell) — CWE-78 safe.
    const findArgs: string[] = [WORKSPACE_ROOT, "-maxdepth", "3", "-type", "f"];
    if (harvestMarker) findArgs.push("-newer", harvestMarker);
    findArgs.push(
      "(",
      "-name", "*.md", "-o", "-name", "*.json", "-o", "-name", "*.csv",
      "-o", "-name", "*.txt", "-o", "-name", "*.html", "-o", "-name", "*.png",
      "-o", "-name", "*.pdf", "-o", "-name", "*.svg", "-o", "-name", "*.yaml",
      "-o", "-name", "*.yml", "-o", "-name", "*.xml",
      ")",
    );
    const found = execFileSync("find", findArgs, {
      encoding: "utf-8",
      timeout: 5000,
      stdio: ["ignore", "pipe", "ignore"],
    })
      .trim()
      .split("\n")
      .slice(0, 50);
    for (const f of found) {
      if (!f) continue;
      const rel = f.replace(`${WORKSPACE_ROOT}/`, "");
      const base = rel.split("/").pop() || rel;
      if (SCAFFOLD_FILES.has(base)) continue;
      if (base.startsWith(".")) continue;
      out.push(rel);
    }
  } catch (e) {
    log.warn(`Artifact harvest failed (continuing): ${(e as Error).message}`);
  }
  return out;
}
