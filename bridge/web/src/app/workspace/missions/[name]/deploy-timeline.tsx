// kars Bridge Workspace — the live Deploy timeline (design note FL/REQ15:
// "dynamically watch agents deploy"). The journey rail shows the high-level
// beat; this fills the Build→Run gap with the granular, real provisioning
// steps so launch is a visible event, not a status flip. Every step is derived
// from real task state (no fabrication): launch approved → sandbox provisioning
// → inference router + access verified → agent online on the mesh → first
// activity → running. Updates with the page's live refresh.

import type { TaskDetail } from "@/lib/types";

type StepState = "done" | "active" | "pending" | "failed";

function dotClass(s: StepState): string {
  return s === "done"
    ? "bg-emerald-500"
    : s === "failed"
      ? "bg-danger"
    : s === "active"
      ? "bg-signal kb-pulse"
      : "bg-surface-muted";
}

export function DeployTimeline({ task }: { task: TaskDetail }) {
  const phase = task.execution_phase;
  const running = phase === "Running";
  const launching = phase === "Launching" || phase === "Pending";
  const hasSandbox = !!task.sandbox;
  const accessVerified = running || phase === "Succeeded";
  const agentOnline = !!task.agent_identity?.last_seen;
  const firstActivity = (task.activity?.length ?? 0) > 0;
  const failed = task.result?.status === "error";
  const succeeded = !failed && (phase === "Succeeded" || !!task.result);

  // Derive each step's state from real signals. A step is "done" once a later
  // signal proves it completed; "active" when it's the current frontier.
  const steps: { label: string; detail: string; state: StepState }[] = [
    {
      label: "Launch approved",
      detail: "You authorized the start — the controller began materializing a sandbox.",
      state: task.launched ? "done" : "pending",
    },
    {
      label: "Sandbox provisioning",
      detail: hasSandbox
        ? `Namespaced sandbox ${task.sandbox} — image pull, seccomp, default-deny egress.`
        : "Creating the isolated namespace, network policy, and seccomp profile.",
      state: hasSandbox && (accessVerified || agentOnline) ? "done" : launching || hasSandbox ? "active" : "pending",
    },
    {
      label: "Inference router + access verified",
      detail: "The governed model path (router sidecar) is up; the composed accesses are enforced at the boundary.",
      state: accessVerified ? "done" : hasSandbox ? "active" : "pending",
    },
    {
      label: "Agent online on the mesh",
      detail: agentOnline
        ? "The agent registered its encrypted mesh identity and is reachable."
        : "Waiting for the agent to register on the encrypted agent mesh.",
      state: agentOnline ? "done" : accessVerified ? "active" : "pending",
    },
    {
      label: "Working",
      detail: failed
        ? "The agent started, but the run terminated before producing a deliverable. Open Run failed for the exact reason."
        : firstActivity
        ? "The agent's loop is live — tool calls and model rounds are streaming into Activity."
        : "Waiting for the first model round / tool call.",
      state: failed ? "failed" : succeeded ? "done" : firstActivity || running ? "active" : "pending",
    },
  ];

  const doneCount = steps.filter((s) => s.state === "done").length;
  const allDone = !failed && (running || succeeded);

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">{failed ? "Run failed" : allDone ? "Running" : "Deploying"}</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            {failed
              ? "Provisioning completed, but the agent run ended before delivery."
              : allDone
              ? "The agent is deployed and working — every step below is verified."
              : "Watch the agent come online — each step is a real, verified provisioning event."}
          </p>
        </div>
        <span
          className={`shrink-0 rounded-full border px-2.5 py-1 text-xs font-medium ${
            failed
              ? "border-danger/30 bg-danger/10 text-danger"
              : allDone
              ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-600"
              : "border-signal/30 bg-signal/10 text-signal"
          }`}
        >
          {failed ? "Failed" : allDone ? "Live" : `${doneCount}/${steps.length}`}
        </span>
      </div>
      <ol className="mt-4 space-y-0">
        {steps.map((s, i) => (
          <li key={s.label} className="flex gap-3">
            <div className="flex flex-col items-center">
              <span className={`mt-1 h-2.5 w-2.5 shrink-0 rounded-full ${dotClass(s.state)}`} aria-hidden />
              {i < steps.length - 1 && (
                <span className={`my-0.5 w-px flex-1 ${s.state === "done" ? "bg-emerald-500/40" : "bg-border"}`} aria-hidden />
              )}
            </div>
            <div className={`pb-4 ${s.state === "pending" ? "opacity-60" : ""}`}>
              <p className="text-sm font-medium leading-tight">
                {s.label}
                {s.state === "active" && (
                  <span className="ml-2 align-middle text-[10px] font-normal text-signal">in progress</span>
                )}
              </p>
              <p className="mt-0.5 text-xs text-foreground-muted">{s.detail}</p>
            </div>
          </li>
        ))}
      </ol>
    </section>
  );
}
