// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { harness, assignment, phase } from "./native-mission.test-support.js";
import { describe, expect, it, vi } from "vitest";
import { createHash } from "node:crypto";
import { readTaskFile, writeTaskFile } from "./task-filesystem.js";
import { authorizeTaskAction } from "./task-policy.js";
import { nativePython, PYTHON_RECEIPTS } from "./native-python.js";

describe("pinned native Mission execution", () => {
  it("reports outstanding phase exports after native writes without substituting inputs or other deliverables", async () => {
    const h = harness();
    h.setProgram(async () => {
      await h.invoke("file_read", { path: "/sandbox/handoff.md" });
      const brief = await h.invoke("file_write", {
        path: "/sandbox/brief.md", content: "Recommendation", artifact_name: "brief.md",
      });
      expect(brief.details).toMatchObject({ artifact_name: "brief.md", required_outputs_remaining: ["handoff.md"],
        required_output_guidance: expect.stringContaining("Do not infer a prohibition from a request to deliver only the final document") });
      expect(JSON.parse(brief.content[0].text)).toEqual(brief.details);
      const handoff = await h.invoke("file_write", {
        path: "/sandbox/handoff.md", content: "Evidence and recommendation for the next phase",
      });
      expect(handoff.details).toMatchObject({ artifact_name: "handoff.md", required_outputs_remaining: [] });
      h.endRound(); h.start();
    });
    const result = await h.execute(assignment({ requiredOutputs: ["handoff.md"] }), () => {}, () => {});
    expect(result.artifacts).toMatchObject({ "brief.md": "Actual file bytes", "handoff.md": "Actual file bytes" });
    expect(result.phase).toMatchObject({ attemptedToolCalls: 3, successfulToolCalls: 3 });
    expect(h.api.runtime.agent.runEmbeddedAgent).toHaveBeenCalledTimes(1);
  });

  it.each([undefined, "r0-p0", "synthesis"].flatMap(slot =>
    ["handoff.md", "mission-acceptance-checklist.md"].map(artifact => ({ slot, artifact }))))(
    "keeps the reviewed export contract when revision feedback corrects the final filename: $slot / $artifact", async ({ slot, artifact }) => {
    const h = harness();
    const content = "Revise mission-acceptance-checklist.md: remove its mistaken handoff.md self-reference. Keep the final concise and save it with the same required name.";
    h.setProgram(async () => {
      await h.invoke("file_write", { path: `/sandbox/${artifact}`, artifact_name: artifact, content: "Phase findings for the corrected checklist" });
    });
    const peerContext = slot ? { rootUid: "reviewed-root", stateName: `kars-reviewed-${"a".repeat(40)}`, slot,
      peers: [{ role: "writer", phase: "draft", slot: "r0-p0", agentName: `rp-${"a".repeat(20)}-r0-p0` }] } : undefined;
    const execution = h.execute(assignment({ content, requiredOutputs: ["handoff.md"],
      ...(peerContext ? { version: 5, peerContext } : {}) }), () => {}, () => {});
    if (artifact === "handoff.md") {
      expect((await execution).artifacts).toEqual({ "handoff.md": "Actual file bytes" });
    } else {
      await expect(execution).rejects.toThrow("Missing required named mission output: handoff.md");
    }
    expect(h.params.prompt).toContain(content);
    expect(h.params.extraSystemPrompt).toContain("References to customer-facing filenames in the charter or revision feedback do not rename or waive this phase's required exports");
    expect(h.params.extraSystemPrompt).toContain("Requests to keep the final filename or deliver only the final document apply to final synthesis; they do not prohibit internal phase handoffs");
    expect(h.params.extraSystemPrompt).toContain('"path":"/sandbox/handoff.md","artifact_name":"handoff.md"');
    if (slot) {
      const stage = slot === "synthesis" ? "Execution stage: final synthesis." : 'Execution stage: internal reviewed phase "r0-p0".';
      expect(h.params.prompt.startsWith(stage)).toBe(true);
      expect(h.params.extraSystemPrompt.startsWith(stage)).toBe(true);
      expect(h.params.extraSystemPrompt).toContain(`Your required ${slot === "synthesis" ? "customer" : "internal"} output names are ["handoff.md"]`);
      if (slot === "synthesis") {
        expect(h.params.extraSystemPrompt).toContain("A previous revision is reference material, not evidence that this run performed its claimed actions");
        expect(h.params.extraSystemPrompt).toContain("Check that the final file, not merely your response, contains the corrections");
      } else {
        expect(h.params.extraSystemPrompt).toContain('Your assigned role is "writer", phase "draft"');
        expect(h.params.extraSystemPrompt).toContain(`your own discovery name is "rp-${"a".repeat(20)}-r0-p0"`);
        expect(h.params.extraSystemPrompt).toContain("That entry is you, not another specialist");
        expect(h.params.extraSystemPrompt).toContain("If multiple fresh registrations match, do not guess");
        expect(h.params.extraSystemPrompt).toContain("Required names are minimum handoff obligations, not an exclusive output list");
        expect(h.params.extraSystemPrompt).toContain("A path or hash in a handoff and an execution receipt do not retain file contents");
        expect(h.params.extraSystemPrompt).toContain("After testing, use kars_mission_file_write with artifact_name for each additional file");
        expect(h.params.extraSystemPrompt).toContain("do not re-export unchanged revision or research inputs");
        expect(h.params.extraSystemPrompt).toContain("Preserve required handoffs, artifact limits, and this phase's authority. Report failed tests as failed");
      }
    } else {
      expect(h.params.extraSystemPrompt).not.toContain("Execution stage:");
    }
    expect(h.params.extraSystemPrompt).toContain("If the assignment explicitly prohibits a required export, report the conflict without writing it or claiming completion");
    expect(writeTaskFile).toHaveBeenCalledTimes(1);
  });

  it("still fails rather than manufacturing a missing phase handoff", async () => {
    const h = harness();
    h.setProgram(async () => {
      const result = await h.invoke("file_write", {
        path: "/sandbox/brief.md", content: "Recommendation", artifact_name: "brief.md",
      });
      expect(result.details).toMatchObject({ required_outputs_remaining: ["handoff.md"] });
    });
    await expect(h.execute(assignment({ requiredOutputs: ["handoff.md"] }), () => {}, () => {}))
      .rejects.toMatchObject({ message: "Missing required named mission output: handoff.md", output: "Report delivered" });
    expect(h.api.runtime.agent.runEmbeddedAgent).toHaveBeenCalledTimes(1);
    expect(writeTaskFile).toHaveBeenCalledTimes(1);
  });

  it("completes 128 reviewed sequential calls with measured rounds and without the old 25-round ceiling", async () => {
    const h = harness();
    h.setProgram(async () => {
      for (let i = 0; i < 128; i++) {
        await h.invoke("file_write", { path: "/sandbox/report.md", content: `Draft ${i}`, artifact_name: "report.md" });
        h.endRound(); h.start();
      }
    });
    const result = await h.execute(assignment({ reviewedPhase: phase({ maxToolCalls: 128 }) }), () => {}, () => {});
    expect(result).toMatchObject({ rounds: 129, phase: { attemptedToolCalls: 128, successfulToolCalls: 128, maxToolCalls: 128 },
      usage: { totalTokens: 129 * 20 }, artifacts: { "report.md": "Actual file bytes" } });
    expect(writeTaskFile).toHaveBeenCalledTimes(128);
    expect(h.api.runtime.agent.runEmbeddedAgent).toHaveBeenCalledTimes(1);
  });

  it.each(["input", "owned", "input-null", "owned-null"])("copies exact %s bytes without model transcription", async variant => {
    const source = variant.split("-")[0];
    const nullable = variant.endsWith("-null");
    const content = "\ufeff# café\r\nprint('雪')\r\n\u0000end";
    const sha256 = `sha256:${createHash("sha256").update(content).digest("hex")}`;
    const h = harness();
    const input = { name: "app.py", content, sha256, source: {
      namespace: "kars-system", taskName: "builder", taskUid: "upstream-task", sandboxUid: "upstream-sandbox",
      podUid: "upstream-pod", runNonce: "upstream-run", assignmentId: "upstream-assignment",
      agentDid: `did:mesh:${"a".repeat(32)}`, artifactName: "app.py",
    } };
    vi.mocked(readTaskFile).mockResolvedValue({ path: "/sandbox/report.md", content,
      bytes: Buffer.byteLength(content), returned_bytes: Buffer.byteLength(content), truncated: false });
    const args = { path: "/sandbox/report.md", source_path: source === "input" ? "/sandbox/.kars-inputs/app.py" : "/sandbox/app.py", sha256,
      ...(nullable ? { content: null, artifact_name: null } : {}) };
    h.setProgram(async () => {
      const schema = h.tools.find(tool => tool.name === "kars_mission_file_write")!.parameters as any;
      expect(schema).not.toHaveProperty("oneOf");
      for (const key of ["content", "source_path", "sha256"]) expect(schema.properties[key].type).toEqual(["string", "null"]);
      if (source === "owned") await h.invoke("file_write", { path: "/sandbox/app.py", content,
        ...(nullable ? { source_path: null, sha256: null, artifact_name: null } : {}) });
      const result = await h.invoke("file_write", args);
      expect(result.details).toMatchObject({ sha256, required_outputs_remaining: [] });
    });
    const result = await h.execute(assignment({ inputArtifacts: source === "input" ? [input] : undefined }), () => {}, () => {});
    expect(Buffer.from(result.artifacts!["report.md"])).toEqual(Buffer.from(content));
    expect(writeTaskFile).toHaveBeenLastCalledWith("/sandbox/report.md", content);
    expect(authorizeTaskAction).toHaveBeenCalledWith("tool:kars_mission_file_write", expect.objectContaining({ arguments: args }));
  });

  it.each(["digest", "ambient", "mixed", "missing-digest", "read-authority", "reserved", "policy", "unobserved", "bundle"])("rejects unsafe copy: %s", async fault => {
    const h = harness();
    const content = "retained source";
    const args: Record<string, unknown> = { path: "/sandbox/report.md", source_path: "/sandbox/source.py",
      sha256: `sha256:${createHash("sha256").update(content).digest("hex")}` };
    if (fault === "digest") args.sha256 = `sha256:${"0".repeat(64)}`;
    if (fault === "mixed") args.content = content;
    if (fault === "missing-digest") delete args.sha256;
    if (fault === "reserved") args.artifact_name = PYTHON_RECEIPTS;
    if (fault === "bundle") args.source_path = "/sandbox/.kars-inputs";
    h.setProgram(async () => {
      if (fault !== "ambient" && fault !== "bundle") await h.invoke("file_write", { path: "/sandbox/source.py", content });
      const count = vi.mocked(writeTaskFile).mock.calls.length;
      if (fault === "policy") vi.mocked(authorizeTaskAction).mockResolvedValue(false);
      await expect(h.invoke("file_write", args, fault !== "unobserved")).rejects.toThrow();
      expect(writeTaskFile).toHaveBeenCalledTimes(count);
    });
    await expect(h.execute(assignment({ reviewedPhase: phase({ capabilities: fault === "read-authority"
      ? ["filesystem-write"] : ["filesystem-write", "filesystem-read"] }) }), () => {}, () => {})).rejects.toThrow();
  });

  it("retains copy failures until retry and checks serialized export capacity before writing", async () => {
    const h = harness();
    const content = "\"".repeat(34000);
    const sha256 = `sha256:${createHash("sha256").update(content).digest("hex")}`;
    h.setProgram(async () => {
      await h.invoke("file_write", { path: "/sandbox/source.py", content });
      await h.invoke("file_write", { path: "/sandbox/first.txt", content: "a".repeat(65000), artifact_name: "first.txt" });
      await expect(h.invoke("file_write", { path: "/sandbox/report.md", source_path: "/sandbox/source.py", sha256 })).rejects.toThrow(/128 KiB/);
      expect(writeTaskFile).toHaveBeenCalledTimes(2);
      await h.invoke("file_write", { path: "/sandbox/report.md", content: "Recovered" });
    });
    const result = await h.execute(assignment(), () => {}, () => {});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 4, successfulToolCalls: 3 });
  });

  it("rejects a seventeenth copied artifact before writing", async () => {
    const h = harness();
    h.setProgram(async () => {
      for (let i = 0; i < 16; i++) await h.invoke("file_write", { path: `/sandbox/${i}.txt`, artifact_name: `${i}.txt`, content: "retained" });
      await expect(h.invoke("file_write", { path: "/sandbox/report.md", source_path: "/sandbox/0.txt",
        sha256: `sha256:${createHash("sha256").update("retained").digest("hex")}` })).rejects.toThrow(/16 files/);
      expect(writeTaskFile).toHaveBeenCalledTimes(16);
    });
    await expect(h.execute(assignment({ reviewedPhase: phase({ maxToolCalls: 20 }) }), () => {}, () => {})).rejects.toThrow(/Unresolved named output/);
  });

  it("rejects copied output mutated before artifact collection", async () => {
    const h = harness();
    h.setProgram(async () => {
      const written = await h.invoke("file_write", { path: "/sandbox/source.py", content: "original" });
      await h.invoke("file_write", { path: "/sandbox/report.md", source_path: "/sandbox/source.py", sha256: (written.details as any).sha256 });
    });
    await expect(h.execute(assignment(), () => {}, () => {})).rejects.toThrow(/changed before artifact collection/);
  });

  it("rejects cumulative serialized exports before writing and permits a shorter retry", async () => {
    const h = harness();
    h.setProgram(async () => {
      await h.invoke("file_write", { path: "/sandbox/first.txt", content: "a".repeat(65000), artifact_name: "first.txt" });
      await expect(h.invoke("file_write", { path: "/sandbox/report.md", content: "\"".repeat(34000) })).rejects.toThrow(/Shorten.*retry/);
      expect(writeTaskFile).toHaveBeenCalledTimes(1);
      await h.invoke("file_write", { path: "/sandbox/report.md", content: "Short recovered report" });
    });
    const result = await h.execute(assignment(), () => {}, () => {});
    expect(result.artifacts).toHaveProperty("report.md");
    expect(result.phase).toMatchObject({ attemptedToolCalls: 3, successfulToolCalls: 2 });
  });

  it("counts every alias when rewriting an exported path without artifact_name", async () => {
    const h = harness();
    h.setProgram(async () => {
      await h.invoke("file_write", { path: "/sandbox/shared.txt", content: "original", artifact_name: "first.txt" });
      await h.invoke("file_write", { path: "/sandbox/shared.txt", content: "original", artifact_name: "report.md" });
      await expect(h.invoke("file_write", { path: "/sandbox/sub/../shared.txt", content: "x".repeat(66000) })).rejects.toThrow(/128 KiB/);
      expect(writeTaskFile).toHaveBeenCalledTimes(2);
      await h.invoke("file_write", { path: "/sandbox/./shared.txt", content: "recovered" });
    });
    const result = await h.execute(assignment(), () => {}, () => {});
    expect(Object.keys(result.artifacts!)).toEqual(["first.txt", "report.md"]);
    expect(result.phase).toMatchObject({ attemptedToolCalls: 4, successfulToolCalls: 3 });
  });

  it("serializes simultaneous writes so only one can consume remaining export capacity", async () => {
    const h = harness();
    h.setProgram(async () => {
      const results = await Promise.allSettled(["a.txt", "b.txt"].map(name => h.invoke("file_write", {
        path: `/sandbox/${name}`, artifact_name: name, content: "x".repeat(70000),
      })));
      expect(results.filter(r => r.status === "fulfilled")).toHaveLength(1);
      expect(results.filter(r => r.status === "rejected")).toHaveLength(1);
      expect(writeTaskFile).toHaveBeenCalledTimes(1);
      const rejected = results.findIndex(r => r.status === "rejected");
      const name = ["a.txt", "b.txt"][rejected];
      await h.invoke("file_write", { path: `/sandbox/${name}`, artifact_name: name, content: "small retry" });
    });
    const result = await h.execute(assignment({ requiredOutputs: ["a.txt", "b.txt"] }), () => {}, () => {});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 3, successfulToolCalls: 2 });
  });

  it("explains Python receipt capacity before execution and permits a shorter export retry", async () => {
    const h = harness();
    const receipt = { ok: true, reason: "exited", exit_code: 0, descendants_cleaned: true };
    vi.mocked(nativePython).mockResolvedValue(receipt);
    h.setProgram(async () => {
      const source = await h.invoke("file_write", { path: "/sandbox/app.py", content: "print(1)" });
      await h.invoke("file_write", { path: "/sandbox/report.md", content: "x".repeat(100000) });
      const request = { entrypoint: "app.py", files: [{ path: "/sandbox/app.py", name: "app.py", sha256: (source.details as { sha256: string }).sha256 }] };
      await expect(h.invoke("python_execute", request)).rejects.toThrow(/Python has not started:.*execution receipt.*Shorten existing exports and retry/);
      expect(nativePython).not.toHaveBeenCalled();
      await h.invoke("file_write", { path: "/sandbox/report.md", content: "Short report; research inputs remain available without republishing." });
      await h.invoke("python_execute", request);
    });
    const result = await h.execute(assignment({ reviewedPhase: phase({ capabilities: ["filesystem-read", "filesystem-write", "network", "python-execute"] }) }), () => {}, () => {});
    expect(nativePython).toHaveBeenCalledTimes(1);
    expect(JSON.parse(result.artifacts![PYTHON_RECEIPTS]).runs).toEqual([receipt]);
    expect(result.phase).toMatchObject({ attemptedToolCalls: 5, successfulToolCalls: 4 });
  });

  it("executes with 90000 bytes of exports and retains the actual failed receipt", async () => {
    const h = harness();
    const files: Record<string, string> = { "/sandbox/report.md": "x".repeat(45000), "/sandbox/research.md": "y".repeat(45000) };
    vi.mocked(readTaskFile).mockImplementation(async path => {
      const content = files[path];
      if (content === undefined) throw new Error(`Unexpected file reread: ${path}`);
      return { path, content, bytes: Buffer.byteLength(content), returned_bytes: Buffer.byteLength(content), truncated: false };
    });
    const receipt = { ok: false, reason: "exited", exit_code: 7, descendants_cleaned: true };
    vi.mocked(nativePython).mockResolvedValue(receipt);
    h.setProgram(async () => {
      const source = await h.invoke("file_write", { path: "/sandbox/app.py", content: "print(1)" });
      await h.invoke("file_write", { path: "/sandbox/report.md", content: "x".repeat(45000) });
      await h.invoke("file_write", { path: "/sandbox/research.md", content: "y".repeat(45000) });
      await h.invoke("python_execute", { entrypoint: "app.py", files: [{ path: "/sandbox/app.py", name: "app.py", sha256: (source.details as { sha256: string }).sha256 }] });
    });
    const result = await h.execute(assignment({ requiredOutputs: ["report.md", "research.md"], reviewedPhase: phase({ capabilities: ["filesystem-read", "filesystem-write", "network", "python-execute"] }) }), () => {}, () => {});
    expect(nativePython).toHaveBeenCalledTimes(1);
    expect(JSON.parse(result.artifacts![PYTHON_RECEIPTS]).runs).toEqual([receipt]);
    expect(result.artifacts).toMatchObject({ "report.md": "x".repeat(45000), "research.md": "y".repeat(45000) });
    expect(result.phase).toMatchObject({ attemptedToolCalls: 4, successfulToolCalls: 3 });
  });

  it("holds concurrent writes until Python finishes and includes its actual receipt", async () => {
    const h = harness();
    let signalStarted!: () => void;
    let complete!: (receipt: Awaited<ReturnType<typeof nativePython>>) => void;
    const started = new Promise<void>(resolve => { signalStarted = resolve; });
    const completed = new Promise<Awaited<ReturnType<typeof nativePython>>>(resolve => { complete = resolve; });
    const receipt = { ok: true, reason: "exited", exit_code: 0, descendants_cleaned: true, stdout: { text: "o".repeat(35000) } };
    vi.mocked(nativePython).mockImplementation(async () => { signalStarted(); return completed; });
    h.setProgram(async () => {
      const source = await h.invoke("file_write", { path: "/sandbox/app.py", content: "print(1)" });
      await h.invoke("file_write", { path: "/sandbox/report.md", content: "x".repeat(80000) });
      const execution = h.invoke("python_execute", { entrypoint: "app.py", files: [{ path: "/sandbox/app.py", name: "app.py", sha256: (source.details as { sha256: string }).sha256 }] });
      await started;
      const write = h.invoke("file_write", { path: "/sandbox/extra.txt", artifact_name: "extra.txt", content: "x".repeat(20000) });
      const rejection = expect(write).rejects.toThrow(/128 KiB/);
      try {
        await vi.waitFor(() => expect(authorizeTaskAction).toHaveBeenCalledWith("tool:kars_mission_file_write", expect.objectContaining({ arguments: expect.objectContaining({ path: "/sandbox/extra.txt" }) })));
        expect(writeTaskFile).toHaveBeenCalledTimes(2);
      } finally { complete(receipt); }
      await execution;
      await rejection;
      await h.invoke("file_write", { path: "/sandbox/extra.txt", artifact_name: "extra.txt", content: "small retry" });
    });
    const result = await h.execute(assignment({ reviewedPhase: phase({ capabilities: ["filesystem-read", "filesystem-write", "network", "python-execute"] }) }), () => {}, () => {});
    expect(JSON.parse(result.artifacts![PYTHON_RECEIPTS]).runs).toEqual([receipt]);
    expect(nativePython).toHaveBeenCalledTimes(1);
  });

  it("rejects a seventeenth pending export before writing without dropping existing files", async () => {
    const h = harness();
    h.setProgram(async () => {
      for (let i = 0; i < 16; i++) await h.invoke("file_write", { path: `/sandbox/${i}.txt`, artifact_name: `${i}.txt`, content: "retained" });
      await expect(h.invoke("file_write", { path: "/sandbox/extra.txt", artifact_name: "extra.txt", content: "extra" })).rejects.toThrow(/16 files/);
      expect(writeTaskFile).toHaveBeenCalledTimes(16);
    });
    await expect(h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ maxToolCalls: 24 }) }), () => {}, () => {})).rejects.toThrow(/Unresolved named output write failure/);
  });

  it("retains final disk reread validation after early export admission", async () => {
    const h = harness();
    h.setProgram(async () => { await h.invoke("file_write", { path: "/sandbox/report.md", content: "small" }); });
    vi.mocked(readTaskFile).mockResolvedValue({ path: "/sandbox/report.md", bytes: 140000, returned_bytes: 131072, content: "x".repeat(131072), truncated: true });
    await expect(h.execute(assignment(), () => {}, () => {})).rejects.toThrow(/exceeds the text artifact bound/);
  });

  it.each([0, 7])("retains machine Python output for actual exit %i using owned source and observed authority", async exit_code => {
    const h = harness();
    const receipt = { ok: exit_code === 0, reason: "exited", exit_code, descendants_cleaned: true, stdout: { text: "actual output" } };
    vi.mocked(nativePython).mockResolvedValue(receipt);
    h.setProgram(async () => {
      const written = await h.invoke("file_write", { path: "/sandbox/app.py", content: "print('actual output')" });
      const { sha256 } = written.details as { sha256: string };
      expect(sha256).toMatch(/^sha256:[a-f0-9]{64}$/);
      const result = await h.invoke("python_execute", { entrypoint: "app.py", files: [{ path: "/sandbox/app.py", name: "app.py", sha256 }] });
      expect(result.details).toMatchObject(receipt);
    });
    const result = await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({
      capabilities: ["filesystem-read", "filesystem-write", "network", "python-execute"],
    }) }), () => {}, () => {});
    expect(JSON.parse(result.artifacts![PYTHON_RECEIPTS]).runs).toEqual([receipt]);
    expect(nativePython).toHaveBeenCalledWith(expect.anything(), ["print('actual output')"],
      ["task-uid", "run-1", "assignment-1", expect.any(String)], expect.any(AbortSignal));
    expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: exit_code === 0 ? 2 : 1 });
    expect(authorizeTaskAction).toHaveBeenCalledWith("tool:kars_mission_python_execute", expect.objectContaining({ assignment_id: "assignment-1" }));
  });

  it("retains Python receipt when the enclosing Mission fails its required output", async () => {
    const h = harness();
    const receipt = { ok: false, reason: "exited", exit_code: 3, descendants_cleaned: true };
    vi.mocked(nativePython).mockResolvedValue(receipt);
    h.setProgram(async () => {
      const written = await h.invoke("file_write", { path: "/sandbox/app.py", content: "raise SystemExit(3)" });
      const { sha256 } = written.details as { sha256: string };
      expect(sha256).toMatch(/^sha256:[a-f0-9]{64}$/);
      await h.invoke("python_execute", { entrypoint: "app.py", files: [{ path: "/sandbox/app.py", name: "app.py", sha256 }] });
    });
    await expect(h.execute(assignment({ reviewedPhase: phase({ capabilities: ["filesystem-read", "filesystem-write", "network", "python-execute"] }) }), () => {}, () => {}))
      .rejects.toMatchObject({ artifacts: { [PYTHON_RECEIPTS]: JSON.stringify({ version: 1, runs: [receipt] }) } });
  });
});
