// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

export const PYTHON_RECEIPTS = "kars-python-executions.json";
export const PYTHON_DESCRIPTION = "Run an exact digest-checked Python entrypoint in the existing Task sandbox as this agent identity, not an inner sandbox. Requires reviewed python-execute, filesystem-read/write and network. Files must be retained inputs or successful writes by this invocation. Fixed Python, literal argv, no shell tool or package installs. Existing router and Pod policy apply. HTTPS_PROXY does not enable opaque HTTPS CONNECT tunnels: those are denied under governed inference. For application-made HTTPS text collection, POST JSON {url, method: \"GET\"} to http://127.0.0.1:8443/egress/fetch from the application. Check the router HTTP result and returned upstream status, headers, body, truncated and observed_at; reject truncated bodies. This existing mediated endpoint retains egress and inference policy, requires no provider credentials, and returns text, not lossless binary ZIP data. Do not disable TLS verification or bypass the router. Separate http_fetch tool calls and fixture tests are not application-made live collection. Limits: 1–60 seconds wall, 30 CPU seconds/process, 512 MiB address space/process, 8 MiB file size, 64 descriptors, and 2048 retained bytes per stream (overflow terminates execution). Stops when more than 64 invocation processes are observed; Pod PID limits remain the hard process bound. Descendants are cleaned; actual exit/failure/stdout/stderr receipts are automatically retained in kars-python-executions.json. Generated workspace files are not automatically exported.";
export const PYTHON_PARAMETERS = { type: "object", additionalProperties: false,
  properties: {
    entrypoint: { type: "string", description: "Relative .py filename listed in files[].name" },
    files: { type: "array", minItems: 1, maxItems: 32, items: { type: "object", additionalProperties: false,
      properties: { path: { type: "string" }, name: { type: "string" }, sha256: { type: "string", pattern: "^sha256:[a-f0-9]{64}$" } },
      required: ["path", "name", "sha256"] } },
    argv: { type: "array", maxItems: 32, items: { type: "string" } },
    timeout_seconds: { type: "integer", minimum: 1, maximum: 60 },
  }, required: ["entrypoint", "files"] };
export interface PythonArguments {
  entrypoint: string;
  files: { path: string; name: string; sha256: string }[];
  argv?: string[];
  timeout_seconds?: number;
}
export interface PythonReceipt extends Record<string, unknown> {
  ok: boolean;
  reason: string;
  exit_code: number | null;
  descendants_cleaned: boolean;
}
const object = (v: unknown): v is Record<string, unknown> => v !== null && typeof v === "object" && !Array.isArray(v);
export const pythonJson = (value: unknown): string => JSON.stringify(value).replace(/[^\x00-\x7f]/g,
  char => `\\u${char.charCodeAt(0).toString(16).padStart(4, "0")}`);
export function pythonArgumentError(value: unknown): string | undefined {
  if (!object(value) || Object.keys(value).some(key => !Object.hasOwn(PYTHON_PARAMETERS.properties, key))
    || !Array.isArray(value.files) || value.files.length < 1 || value.files.length > 32) return "Supply only declared arguments and 1–32 assignment-owned files";
  const names = new Set<string>();
  for (const item of value.files) {
    if (!object(item) || Object.keys(item).sort().join() !== "name,path,sha256"
      || typeof item.path !== "string" || !item.path.length || item.path.length > 4096 || item.path.includes("\0")
      || typeof item.name !== "string" || item.name.length > 128 || names.has(item.name)
      || !item.name.split("/").every(part => /^[A-Za-z0-9_][A-Za-z0-9._-]*$/.test(part))
      || typeof item.sha256 !== "string" || !/^sha256:[a-f0-9]{64}$/.test(item.sha256)) return "Invalid or duplicate source manifest entry";
    names.add(item.name);
  }
  if ([...names].some(name => name.split("/").some((_, i, parts) => i > 0 && names.has(parts.slice(0, i).join("/"))))) return "Source conflicts with a parent directory";
  if (Buffer.byteLength(pythonJson(value.files)) > 8192) return "Source manifest exceeds 8192 bytes";
  if (typeof value.entrypoint !== "string" || !names.has(value.entrypoint) || !value.entrypoint.endsWith(".py")) return "Entrypoint must be a supplied .py file";
  const argv = value.argv ?? [];
  if (!Array.isArray(argv) || argv.length > 32 || argv.some(arg => typeof arg !== "string" || Buffer.byteLength(arg) > 1024 || arg.includes("\0"))
    || Buffer.byteLength(pythonJson(argv)) > 4096) return "argv must contain bounded literal strings";
  if (value.timeout_seconds !== undefined && (!Number.isSafeInteger(value.timeout_seconds) || (value.timeout_seconds as number) < 1 || (value.timeout_seconds as number) > 60)) return "timeout_seconds must be an integer from 1 to 60";
  return undefined;
}

export function pythonReceipts(receipts: readonly PythonReceipt[]): string {
  const content = pythonJson({ version: 1, runs: receipts });
  if (Buffer.byteLength(content) > 65536) throw new Error("Insufficient Python receipt artifact capacity; finish this phase before another execution");
  return content;
}

export function pythonReceiptReservation(receipts: readonly PythonReceipt[], args: PythonArguments, identity: string[]): string {
  // One replacement character per retained byte bounds even invalid UTF-8 in both JSON layers.
  const stream = { text: "\ufffd".repeat(2048), base64: "A".repeat(2732), sha256: `sha256:${"a".repeat(64)}`,
    observed_bytes: Number.MAX_SAFE_INTEGER, truncated: false };
  const worst: PythonReceipt = { version: 1, identity, entrypoint: args.entrypoint, files: args.files,
    argv: args.argv ?? [], timeout_seconds: args.timeout_seconds ?? 30, elapsed_ms: Number.MAX_SAFE_INTEGER,
    reason: "cleanup_failed", exit_code: -2147483648, descendants_cleaned: false,
    network: "existing-agent-governed-proxy; no claim of live collection", stdout: stream, stderr: stream, ok: false };
  const current = pythonReceipts(receipts);
  const future = pythonReceipts([...receipts, worst]);
  const raw = future.length - current.length;
  const escaped = JSON.stringify(future).length - JSON.stringify(current).length - raw;
  // Reserve both exact bounds with JSON whitespace, never a fabricated execution receipt.
  return current + " ".repeat(raw - escaped) + "\t".repeat(escaped);
}

export async function nativePython(args: PythonArguments, contents: string[], identity: string[], signal: AbortSignal): Promise<PythonReceipt> {
  const error = pythonArgumentError(args);
  if (error) throw new Error(error);
  signal.throwIfAborted();
  return new Promise((resolve, reject) => {
    const child = spawn("/usr/bin/python3", ["-I", "-S", "-B", fileURLToPath(new URL("./native-python.py", import.meta.url))],
      { env: {}, stdio: ["pipe", "pipe", "ignore"] });
    const chunks: Buffer[] = [];
    let bytes = 0;
    let failure: Error | undefined;
    let killTimer: ReturnType<typeof setTimeout> | undefined;
    const cancel = () => {
      child.kill("SIGTERM");
      killTimer ??= setTimeout(() => { failure ??= new Error("Python supervisor did not confirm descendant cleanup; Task must stop"); child.kill("SIGKILL"); }, 6000);
    };
    const timer = setTimeout(() => { failure = new Error("Python supervisor exceeded its wall/cleanup allowance"); cancel(); }, ((args.timeout_seconds ?? 30) + 10) * 1000);
    signal.addEventListener("abort", cancel, { once: true });
    child.stdout.on("data", (chunk: Buffer) => {
      bytes += chunk.length;
      if (bytes > 128 * 1024) { failure = new Error("Python supervisor response exceeded bound"); cancel(); }
      else chunks.push(chunk);
    });
    child.stdin.on("error", error => { failure = error; cancel(); });
    child.on("error", error => { failure = error; });
    child.on("close", code => {
      clearTimeout(timer);
      if (killTimer) clearTimeout(killTimer);
      signal.removeEventListener("abort", cancel);
      try {
        if (failure) throw failure;
        const result: unknown = JSON.parse(Buffer.concat(chunks).toString("utf8"));
        if (code !== 0 || !object(result) || typeof result.ok !== "boolean" || typeof result.reason !== "string"
          || typeof result.descendants_cleaned !== "boolean" || !(result.exit_code === null || Number.isInteger(result.exit_code))) {
          throw new Error(`Python supervisor unavailable: ${object(result) ? String(result.error ?? "invalid result") : "invalid result"}`);
        }
        resolve(result as PythonReceipt);
      } catch (error) { reject(error); }
    });
    child.stdin.end(JSON.stringify({ arguments: args, contents, identity, parent_pid: process.pid }));
    if (signal.aborted) cancel();
  });
}
