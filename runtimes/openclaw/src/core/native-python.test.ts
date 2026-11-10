// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { nativePython, pythonArgumentError, pythonJson, pythonReceipts, pythonReceiptReservation } from "./native-python.js";
import { TaskPhaseGuard } from "./task-phase.js";

const args = { entrypoint: "app.py", files: [{ path: "/sandbox/app.py", name: "app.py", sha256: `sha256:${"a".repeat(64)}` }] };
describe("explicit native Python authority", () => {
  it.each([
    { entrypoint: "../app.py" }, { entrypoint: "app.sh" }, { timeout_seconds: true }, { timeout_seconds: 61 },
    { argv: ["bad\0arg"] }, { argv: ["x".repeat(1025)] }, { command: "python app.py" }, { files: [] },
    { files: [args.files[0], args.files[0]] },
    { files: [{ ...args.files[0], name: "../app.py" }] },
  ])("rejects invalid or shell-shaped arguments %j", invalid => {
    expect(pythonArgumentError({ ...args, ...invalid })).toBeTypeOf("string");
  });
  it("keeps literal arguments and the cross-runtime ASCII JSON bound", () => {
    expect(pythonArgumentError({ ...args, argv: ["$(not-a-shell)", "; literal"] })).toBeUndefined();
    expect(pythonJson(["☃", "🔥"])).toBe('["\\u2603","\\ud83d\\udd25"]');
    expect(pythonArgumentError({ ...args, argv: ["🔥".repeat(250), "🔥".repeat(250)] })).toContain("bounded");
    expect(() => pythonReceiptReservation([{ ok: true, reason: "exited", exit_code: 0, descendants_cleaned: true, output: "x".repeat(40000) }], args, ["task", "run", "assignment", "call"])).toThrow("capacity");
    expect(() => pythonReceipts([{ ok: true, reason: "exited", exit_code: 0, descendants_cleaned: true, output: "x".repeat(65536) }])).toThrow("capacity");
  });
  it("matches both shared supervisors without publishing a synthetic run in the reservation", () => {
    const previous = [{ ok: false, reason: "exited", exit_code: 7, descendants_cleaned: true, stdout: { text: "retained ☃\n" } }];
    const identity = ["task", "run", "assignment", "call"];
    const requests = [args, { ...args, files: [{ ...args.files[0], path: "\\\"☃".repeat(200) }], argv: ["\\\"☃".repeat(100)] }];
    const openclaw = new URL("./native-python.py", import.meta.url);
    const hermes = new URL("../../../hermes/src/kars_runtime_hermes/plugin/native_python.py", import.meta.url);
    expect(readFileSync(openclaw)).toEqual(readFileSync(hermes));
    for (const request of requests) {
      expect(pythonArgumentError(request)).toBeUndefined();
      const reservation = pythonReceiptReservation(previous, request, identity);
      expect(JSON.parse(reservation)).toEqual({ version: 1, runs: previous });
      const result = spawnSync("python3", ["-I", "-S", "-B", "-c", [
        "import importlib.util,json,sys",
        "spec=importlib.util.spec_from_file_location('supervisor',sys.argv[1])",
        "module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)",
        "value=json.load(sys.stdin)",
        "sys.stdout.write(module.reserve_receipt(value['previous'],value['args'],value['identity']))",
      ].join("\n"), fileURLToPath(openclaw)], { input: JSON.stringify({ previous, args: request, identity }), encoding: "utf8" });
      expect(result.status, result.stderr).toBe(0);
      expect(reservation).toBe(result.stdout);
      expect(reservation.length).toBeLessThanOrEqual(65536);
      expect(reservation.length).toBeLessThan(pythonReceipts(previous).length + 48000);
    }
  });
  it("does not under-reserve double-escaped maximum request metadata", () => {
    const request = { ...args, files: [{ ...args.files[0], path: "\ufffd".repeat(1300) }], argv: Array(4).fill("\ufffd".repeat(150)) };
    const identity = Array(4).fill("\ufffd".repeat(80));
    expect(pythonArgumentError(request)).toBeUndefined();
    expect(pythonJson(identity).length).toBeLessThanOrEqual(2048);
    const reservation = pythonReceiptReservation([], request, identity);
    expect(reservation.length).toBeLessThan(65536);
    expect(JSON.stringify(reservation).length).toBeGreaterThan(JSON.stringify(pythonReceipts([]) + " ".repeat(48000)).length);
    expect(JSON.parse(reservation).runs).toEqual([]);
  });
  it.each([0, 9, 34, 92, 127, 128, 255])("reserves raw and outer JSON for full byte-%i streams and failed receipts", byte => {
    const request = { ...args, argv: ["literal\\\"☃"] };
    const identity = ["task", "run", "assignment", "call"];
    const previous = [{ ok: false, reason: "timeout", exit_code: -9, descendants_cleaned: true }];
    const reservation = pythonReceiptReservation(previous, request, identity);
    const bytes = Buffer.alloc(2048, byte);
    const stream = { text: bytes.toString("utf8"), base64: bytes.toString("base64"), sha256: `sha256:${"b".repeat(64)}`,
      observed_bytes: 6144, truncated: true };
    const actual = pythonReceipts([...previous, { version: 1, identity, entrypoint: request.entrypoint, files: request.files,
      argv: request.argv, timeout_seconds: 30, elapsed_ms: 70000, reason: "output_limit", exit_code: -9,
      descendants_cleaned: true, network: "existing-agent-governed-proxy; no claim of live collection",
      stdout: stream, stderr: stream, ok: false }]);
    expect(Buffer.byteLength(actual)).toBeLessThanOrEqual(Buffer.byteLength(reservation));
    expect(Buffer.byteLength(JSON.stringify(actual))).toBeLessThanOrEqual(Buffer.byteLength(JSON.stringify(reservation)));
    expect(JSON.parse(reservation).runs).toEqual(previous);
  });
  it("requires explicit Python plus both filesystem permissions and network, never shell", () => {
    const phase = { name: "tester", objective: "Execute exact delivered Python source", executionMode: "native", maxToolCalls: 4,
      capabilities: ["filesystem-read", "filesystem-write", "network", "python-execute"] };
    expect(new TaskPhaseGuard(phase).allowsTool("python_execute")).toBe(true);
    expect(new TaskPhaseGuard(phase).allowsTool("shell")).toBe(false);
    for (const missing of phase.capabilities.slice(0, 3))
      expect(() => new TaskPhaseGuard({ ...phase, capabilities: phase.capabilities.filter(c => c !== missing) })).toThrow();
    expect(() => new TaskPhaseGuard({ ...phase, executionMode: undefined })).toThrow();
    expect(new TaskPhaseGuard({ ...phase, capabilities: phase.capabilities.slice(0, 3) }).allowsTool("python_execute")).toBe(false);
  });
  it("does not launch anything for pre-cancelled calls", async () => {
    await expect(nativePython(args, ["pass"], ["task", "run", "assignment", "call"], AbortSignal.abort())).rejects.toThrow();
  });
});
