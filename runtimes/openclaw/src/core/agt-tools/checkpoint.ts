// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Durable milestone checkpoint tool for the native OpenClaw harness.

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyApi = any;

export function registerCheckpointTool(
  api: AnyApi,
  reportProgress: (stage: string, details?: Record<string, unknown>) => void,
): void {
  api.registerTool({
    name: "checkpoint",
    label: "Durable Milestone Checkpoint",
    description:
      "Persist a resumable milestone checkpoint. Use when a milestone starts, completes, blocks, or hands off. The controller stores the latest checkpoint and injects it into replacement workers.",
    parameters: {
      type: "object",
      properties: {
        milestone_id: { type: "string" },
        status: {
          type: "string",
          enum: ["pending", "in_progress", "completed", "blocked"],
        },
        summary: { type: "string" },
        acceptance_criteria: { type: "array", items: { type: "string" } },
        artifacts: { type: "array", items: { type: "string" } },
        next_steps: { type: "array", items: { type: "string" } },
      },
      required: ["milestone_id", "status", "summary"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      const milestoneId = String(params.milestone_id || "")
        .trim()
        .replace(/[^a-zA-Z0-9._-]/g, "-")
        .slice(0, 96);
      const status = String(params.status || "").trim();
      const summary = String(params.summary || "").trim().slice(0, 2_000);
      if (
        !milestoneId
        || !["pending", "in_progress", "completed", "blocked"].includes(status)
        || !summary
      ) {
        return {
          content: [{
            type: "text",
            text: "checkpoint error: milestone_id, valid status, and summary are required",
          }],
          isError: true,
        };
      }

      const fs = await import("node:fs");
      const workspaceRoot =
        process.env.KARS_WORKSPACE_ROOT || "/sandbox/.openclaw/workspace";
      const checkpointPath = `${workspaceRoot}/task-checkpoint.json`;
      const checkpoint = {
        schema: "kars.checkpoint/v1",
        milestone_id: milestoneId,
        status,
        summary,
        acceptance_criteria: Array.isArray(params.acceptance_criteria)
          ? params.acceptance_criteria.map(String).slice(0, 20)
          : [],
        artifacts: Array.isArray(params.artifacts)
          ? params.artifacts.map(String).slice(0, 50)
          : [],
        next_steps: Array.isArray(params.next_steps)
          ? params.next_steps.map(String).slice(0, 20)
          : [],
        updated_at: new Date().toISOString(),
        agent: process.env.SANDBOX_NAME || process.env.HOSTNAME || "unknown",
      };
      fs.mkdirSync(workspaceRoot, { recursive: true });
      fs.writeFileSync(`${checkpointPath}.tmp`, JSON.stringify(checkpoint, null, 2), {
        mode: 0o600,
      });
      fs.renameSync(`${checkpointPath}.tmp`, checkpointPath);
      reportProgress("checkpoint", { checkpoint });
      return {
        content: [{
          type: "text",
          text: JSON.stringify({ persisted: true, path: checkpointPath, checkpoint }),
        }],
      };
    },
  });
}
