// AGT offload executor — extracted from plugin.ts in S15.f.5.
//
// Two functions:
//   - runOffloadTask: executes one offloaded task against the in-process
//     tool-calling loop, ships heartbeat pings + output files + a final
//     offload_done/offload_error back to the parent over the mesh.
//   - startProactiveOffloadIfNeeded: at boot, when OFFLOAD_REQUEST_ID is
//     present, announces offload_hello and kicks off runOffloadTask in
//     the background.
//
// Both take a `deps` bag because they touch a slice of the AGT singleton
// state owned by plugin.ts (mesh client, identity, sandbox name, the
// in-flight set, the in-process processTaskWithTools handler, and the
// chunked-aware meshSend wrapper).

import { nameToAmid } from "./amid-cache.js";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyMeshClient = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyMeshIdentity = any;
type Logger = { info: (m: string) => void; warn: (m: string) => void };

export interface OffloadDeps {
  meshClient: AnyMeshClient;
  identity: AnyMeshIdentity;
  sandboxName: string;
  isConnected: () => boolean;
  offloadInFlight: Set<string>;
  meshSend: (
    client: AnyMeshClient,
    target: string,
    message: Record<string, unknown>,
    log?: Logger,
  ) => Promise<string | undefined>;
  processTaskWithTools: (taskContent: unknown, log: Logger) => Promise<string>;
}

export interface RunOffloadOpts {
  requestId: string;
  parentAmid: string;
  parentName: string;
  task: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  files: any[];
  source: "env" | "message";
}

export async function runOffloadTask(
  opts: RunOffloadOpts,
  deps: OffloadDeps,
  log: Logger,
): Promise<void> {
  const { requestId, parentAmid, parentName, task, files, source } = opts;
  const startTime = Date.now();
  const fromAgent = deps.sandboxName || process.env.SANDBOX_NAME || "sandbox";

  log.info(
    `☁️ Offload task ${source === "env" ? "auto-started from env" : "received from '" + parentName + "'"} ` +
    `(${parentAmid.slice(0, 12)}...) — request: ${requestId.slice(0, 8)}, ` +
    `task: ${String(task).slice(0, 100)}, files: ${files.length}`
  );

  // Heartbeat — periodic offload_progress pings while task is running.
  let heartbeatTick = 0;
  const heartbeatTimer = setInterval(() => {
    heartbeatTick += 1;
    const elapsedSec = Math.round((Date.now() - startTime) / 1000);
    deps.meshSend(deps.meshClient, parentAmid, {
      type: "offload_progress",
      request_id: requestId,
      stage: "executing",
      message: `Task in progress (${elapsedSec}s elapsed, tick ${heartbeatTick})`,
      pct: Math.min(10 + heartbeatTick * 2, 85),
      elapsed_seconds: elapsedSec,
      from_agent: fromAgent,
      timestamp: new Date().toISOString(),
    }, log).catch(() => { /* best-effort heartbeat */ });
  }, 20_000);

  // Initial progress: task started
  try {
    await deps.meshSend(deps.meshClient, parentAmid, {
      type: "offload_progress",
      request_id: requestId,
      stage: "executing",
      message: "Task execution started",
      pct: 10,
      from_agent: fromAgent,
      timestamp: new Date().toISOString(),
    }, log);
  } catch { /* best effort */ }

  // mkdtempSync + crypto-random filename to avoid predictable tmp paths
  // (CWE-377: insecure-temp-file).
  let harvestMarker = "";
  try {
    const fs = await import("node:fs");
    const os = await import("node:os");
    const path = await import("node:path");
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "offload-"));
    harvestMarker = path.join(tmpDir, "start");
    fs.writeFileSync(harvestMarker, "", { mode: 0o600 });
  } catch { /* best effort */ }

  // Guarded task content — prevents the inner LLM from re-delegating.
  const guardedTask = [
    "You are running INSIDE an AzureClaw offload sandbox — you ARE the cloud",
    "executor. Do NOT try to delegate this task to another sandbox; calls to",
    "cloud_offload, azureclaw_spawn, or handoff will be policy-denied and",
    "will fail. Execute the task directly HERE.",
    "",
    "Write ALL artifacts (markdown, JSON, CSV, HTML, PNG, PDF, TXT) to",
    "/sandbox/.openclaw/workspace/ using the `file_write` tool — files in",
    "that directory are automatically shipped back to the parent when the",
    "task completes. DO NOT use shell redirection (`cat > file <<EOF`,",
    "`echo > file`, etc.) — the sandbox shell policy blocks it. Example:",
    "  file_write(path=\"/sandbox/.openclaw/workspace/report.md\", content=\"...\")",
    "",
    "If the task text below mentions 'offload' or a 'cloud sandbox', IGNORE",
    "that framing — you ARE the cloud sandbox. Just do the work and produce",
    "the requested file(s).",
    "",
    "TASK:",
    String(task),
  ].join("\n");

  let taskResult: string;
  let taskSuccess = true;
  try {
    taskResult = await deps.processTaskWithTools(guardedTask, log);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
  } catch (taskErr: any) {
    taskResult = `Task execution failed: ${taskErr.message}`;
    taskSuccess = false;
  }

  clearInterval(heartbeatTimer);

  const duration = Math.round((Date.now() - startTime) / 1000);

  // Collect output files — any new artifacts in the workspace.
  const outputFiles: string[] = [];
  try {
    const { execFileSync } = await import("node:child_process");
    const workspaceRoot = "/sandbox/.openclaw/workspace";
    const SCAFFOLD_FILES = new Set([
      "USER.md", "SOUL.md", "AGENTS.md", "TOOLS.md", "MEMORY.md",
      "HEARTBEAT.md", "IDENTITY.md", "workspace-state.json",
    ]);
    // execFileSync with arg array (no shell) — CWE-78 / js/indirect-command-line-injection.
    const findArgs: string[] = [workspaceRoot, "-maxdepth", "3", "-type", "f"];
    if (harvestMarker) findArgs.push("-newer", harvestMarker);
    findArgs.push(
      "(",
      "-name", "*.md", "-o", "-name", "*.json", "-o", "-name", "*.csv",
      "-o", "-name", "*.txt", "-o", "-name", "*.html", "-o", "-name", "*.png",
      "-o", "-name", "*.pdf", "-o", "-name", "*.svg", "-o", "-name", "*.yaml",
      "-o", "-name", "*.yml", "-o", "-name", "*.xml",
      ")",
    );
    const newFiles = execFileSync("find", findArgs, {
      encoding: "utf-8",
      timeout: 5000,
      stdio: ["ignore", "pipe", "ignore"],
    }).trim().split("\n").slice(0, 50).join("\n");
    if (newFiles) {
      for (const f of newFiles.split("\n")) {
        if (!f) continue;
        const rel = f.replace(`${workspaceRoot}/`, "");
        const base = rel.split("/").pop() || rel;
        if (SCAFFOLD_FILES.has(base)) continue;
        if (base.startsWith(".")) continue;
        outputFiles.push(rel);
      }
    }
  } catch { /* no output files — that's fine */ }

  // Fallback: textual response without a workspace file → save as markdown.
  if (taskSuccess && outputFiles.length === 0 && taskResult && taskResult.length > 400) {
    try {
      const fs = await import("node:fs");
      const fallbackName = `offload-${requestId.slice(0, 8)}-response.md`;
      const fallbackPath = `/sandbox/.openclaw/workspace/${fallbackName}`;
      fs.mkdirSync("/sandbox/.openclaw/workspace", { recursive: true });
      fs.writeFileSync(fallbackPath, taskResult, "utf-8");
      outputFiles.push(fallbackName);
      log.info(
        `📝 No explicit output files — saved agent response as fallback ` +
        `deliverable: ${fallbackName} (${taskResult.length} chars)`
      );
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } catch (fbErr: any) {
      log.warn(`Failed to write fallback response file: ${fbErr.message}`);
    }
  }

  // Clean up the harvest marker.
  try {
    if (harvestMarker) {
      const fs = await import("node:fs");
      const path = await import("node:path");
      fs.unlinkSync(harvestMarker);
      try { fs.rmdirSync(path.dirname(harvestMarker)); } catch { /* ignore */ }
    }
  } catch { /* ignore */ }

  // Send output files back to requester via file_transfer.
  for (const relPath of outputFiles.slice(0, 10)) {
    try {
      const fPath = `/sandbox/.openclaw/workspace/${relPath}`;
      const fs = await import("node:fs");
      // Open once to avoid stat→read race (CWE-367 TOCTOU).
      const fd = fs.openSync(fPath, "r");
      let fStat: import("node:fs").Stats;
      let fData: Buffer;
      try {
        fStat = fs.fstatSync(fd);
        if (fStat.size > 30 * 1024 * 1024) { fs.closeSync(fd); continue; }
        fData = Buffer.alloc(fStat.size);
        fs.readSync(fd, fData, 0, fStat.size, 0);
      } finally {
        fs.closeSync(fd);
      }
      const fName = relPath.split("/").pop() || relPath;
      await deps.meshSend(deps.meshClient, parentAmid, {
        type: "file_transfer",
        file_name: fName,
        file_path: relPath,
        file_data: fData.toString("base64"),
        size_bytes: fStat.size,
        description: `Output file from offload ${requestId.slice(0, 8)}`,
        from_agent: fromAgent,
        timestamp: new Date().toISOString(),
      }, log);
      log.info(`📁 Sent output file '${fName}' (${(fStat.size / 1024).toFixed(1)} KB) to '${parentName}'`);
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } catch (ftErr: any) {
      log.warn(`Failed to send output file '${relPath}': ${ftErr.message}`);
    }
  }

  // Final offload_done / offload_error.
  if (taskSuccess) {
    await deps.meshSend(deps.meshClient, parentAmid, {
      type: "offload_done",
      request_id: requestId,
      summary: taskResult.slice(0, 8000),
      output_files: outputFiles,
      output_file_contents: [],
      tokens_used: { prompt: 0, completion: 0 },
      duration_seconds: duration,
      from_agent: fromAgent,
      timestamp: new Date().toISOString(),
    }, log);
    log.info(
      `✅ Offload complete: ${requestId.slice(0, 8)} — ` +
      `${duration}s, ${outputFiles.length} output file(s)`
    );
  } else {
    await deps.meshSend(deps.meshClient, parentAmid, {
      type: "offload_error",
      request_id: requestId,
      error: taskResult.slice(0, 4000),
      phase: "execution",
      from_agent: fromAgent,
      timestamp: new Date().toISOString(),
    }, log);
    log.info(`❌ Offload failed: ${requestId.slice(0, 8)} — ${duration}s`);
  }
}

/**
 * If OFFLOAD_REQUEST_ID/OFFLOAD_PARENT_AMID/OFFLOAD_TASK are present in the
 * environment, this sandbox is a controller-spawned offload worker. Announce
 * offload_hello to the parent and start runOffloadTask in the background.
 */
export async function startProactiveOffloadIfNeeded(
  deps: OffloadDeps,
  log: Logger,
): Promise<void> {
  const requestId = process.env.OFFLOAD_REQUEST_ID || "";
  const parentAmid = process.env.OFFLOAD_PARENT_AMID || "";
  const taskRaw = process.env.OFFLOAD_TASK || "";

  if (!requestId || !parentAmid || !taskRaw) return;
  if (!deps.meshClient || !deps.isConnected()) {
    log.warn(
      `⚠️ Proactive offload: mesh not ready — cannot announce ` +
      `(requestId=${requestId.slice(0, 8)}, parent=${parentAmid.slice(0, 12)}...)`
    );
    return;
  }
  if (deps.offloadInFlight.has(requestId)) return;

  // OFFLOAD_TASK may be raw text or a JSON envelope {task, files, parent_name}.
  let task = taskRaw;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let files: any[] = [];
  let parentName = "parent";
  try {
    const parsed = JSON.parse(taskRaw);
    if (parsed && typeof parsed === "object") {
      if (typeof parsed.task === "string") task = parsed.task;
      if (Array.isArray(parsed.files)) files = parsed.files;
      if (typeof parsed.parent_name === "string") parentName = parsed.parent_name;
    }
  } catch { /* treat as raw text */ }

  const fromAgent = deps.sandboxName || process.env.SANDBOX_NAME || "sandbox";

  // Seed name→AMID for the literal alias 'parent' so sub-agent tool calls
  // like mesh_send(to_agent='parent') resolve without registry lookup.
  nameToAmid.set("parent", parentAmid);
  if (parentName && parentName !== "parent") {
    nameToAmid.set(parentName, parentAmid);
  }

  // 1. offload_hello.
  try {
    await deps.meshClient.send(parentAmid, {
      type: "offload_hello",
      request_id: requestId,
      from_agent: fromAgent,
      task_preview: String(task).slice(0, 200),
      started_at: new Date().toISOString(),
      message: `Offload sandbox '${fromAgent}' online — starting task ${requestId.slice(0, 8)}`,
    });
    log.info(
      `☁️ Proactive offload announced to parent (${parentAmid.slice(0, 12)}...) — ` +
      `request: ${requestId.slice(0, 8)}`
    );
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
  } catch (helloErr: any) {
    log.warn(
      `Proactive offload: failed to send offload_hello (${helloErr.message}) — ` +
      `continuing with execution, parent may rely on fallback flow`
    );
  }

  // 2. Run task in background.
  deps.offloadInFlight.add(requestId);
  runOffloadTask(
    { requestId, parentAmid, parentName, task, files, source: "env" },
    deps,
    log,
  )
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    .catch((err: any) => log.warn(`Proactive offload task crashed: ${err.message}`))
    .finally(() => deps.offloadInFlight.delete(requestId));
}
