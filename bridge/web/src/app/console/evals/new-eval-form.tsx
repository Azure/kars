// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// kars Bridge Operator Console — configure + launch a safety eval. Operator-only:
// pick a running sandbox and an adversarial corpus, and the controller spawns the
// runner Job that replays it and records a real verdict. Re-running the same
// sandbox+corpus updates the existing eval in place.

import { useState, type FormEvent } from "react";
import { useRouter } from "next/navigation";

const BUILTIN_CORPORA = [
  "jailbreak-baseline",
  "prompt-injection-2026q1",
  "banned-tools",
  "egress-known-bad",
  "governed-egress-tunnels",
  "memory-isolation",
];

export function NewEvalForm({
  sandboxes,
  defaultRunnerImage = "",
}: {
  sandboxes: { name: string; runtime: string | null }[];
  defaultRunnerImage?: string;
}) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [target, setTarget] = useState(sandboxes[0]?.name ?? "");
  const [corpus, setCorpus] = useState(BUILTIN_CORPORA[0]);
  const [schedule, setSchedule] = useState("");
  const [runnerImage, setRunnerImage] = useState(defaultRunnerImage);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const targetAvailable = sandboxes.some((sandbox) => sandbox.name === target);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    if (!targetAvailable) {
      setError("The selected sandbox is no longer available for a new eval. Choose a running target.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const res = await fetch("/api/operator/evals", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          target_sandbox: target,
          corpus,
          schedule: schedule.trim() || null,
          runner_image: runnerImage.trim() || null,
          run_now: true,
        }),
      });
      if (!res.ok) {
        const b = await res.json().catch(() => null);
        throw new Error(b?.error?.message ?? `Create failed (${res.status})`);
      }
      setOpen(false);
      router.refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Create failed");
    } finally {
      setBusy(false);
    }
  }

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        disabled={sandboxes.length === 0}
        className="rounded-lg bg-signal px-3 py-1.5 text-xs font-semibold text-signal-fg transition hover:opacity-90 disabled:opacity-50"
        title={sandboxes.length === 0 ? "No running sandbox to evaluate" : "Configure and launch a safety eval"}
      >
        + New eval
      </button>
    );
  }

  return (
    <form onSubmit={submit} aria-busy={busy} className="rounded-xl border border-border bg-surface p-5">
      <h2 className="text-sm font-semibold">Configure a safety eval</h2>
      <p className="mt-0.5 text-xs text-foreground-muted">
        Replays an adversarial corpus against the sandbox&apos;s inference router and records a real
        pass/fail verdict.
      </p>
      <fieldset disabled={busy} className="mt-3 grid gap-3 sm:grid-cols-2">
        <label className="block">
          <span className="text-xs font-medium text-foreground-muted">Target sandbox</span>
          <select
            required
            value={targetAvailable ? target : ""}
            onChange={(e) => setTarget(e.target.value)}
            className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
          >
            <option value="" disabled>Select a running sandbox</option>
            {sandboxes.map((s) => (
              <option key={s.name} value={s.name}>
                {s.name}{s.runtime ? ` · ${s.runtime}` : ""}
              </option>
            ))}
          </select>
        </label>
        <label className="block">
          <span className="text-xs font-medium text-foreground-muted">Corpus</span>
          <input
            list="corpus-options"
            required
            value={corpus}
            onChange={(e) => setCorpus(e.target.value)}
            className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm font-mono"
          />
          <datalist id="corpus-options">
            {BUILTIN_CORPORA.map((c) => (
              <option key={c} value={c} />
            ))}
          </datalist>
        </label>
        <label className="block">
          <span className="text-xs font-medium text-foreground-muted">Schedule (cron, optional)</span>
          <input
            value={schedule}
            onChange={(e) => setSchedule(e.target.value)}
            placeholder="0 */6 * * *  (leave blank for one-shot)"
            className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm font-mono"
          />
        </label>
        <label className="block">
          <span className="text-xs font-medium text-foreground-muted">Runner image override (optional)</span>
          <input
            value={runnerImage}
            onChange={(e) => setRunnerImage(e.target.value)}
            placeholder="Use the Core-configured runner"
            className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm font-mono"
          />
          <span className="mt-1 block text-xs text-foreground-muted">
            Leave blank to use the runner configured by your Core installation.
          </span>
        </label>
      </fieldset>
      {corpus.trim() === "egress-known-bad" && (
        <p className="mt-3 text-xs text-foreground-muted">
          This corpus expects strict egress enforcement with none of its probe destinations allowed.
          Check the target&apos;s policy first: Learn mode or approved destinations can produce
          mismatches or inconclusive results. Task-bound governed inference rejects opaque tunnels
          before the allowlist runs, even with an unbounded allowance; use governed-egress-tunnels
          for that boundary instead. Calendar-only accounting does not enable this tunnel fence.
          Transport errors are not passing policy verdicts.
        </p>
      )}
      {corpus.trim() === "governed-egress-tunnels" && (
        <p className="mt-3 text-xs text-foreground-muted">
          Requires task-bound governed inference, including unbounded allowances. Calendar-only
          accounting is not sufficient: its proxy still evaluates destination policies. All six
          destinations must return the explicit opaque-tunnel refusal. This does not test
          destination-allowlist enforcement; generic denials and transport errors do not pass.
        </p>
      )}
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <button
          type="submit"
          disabled={busy || !targetAvailable || !corpus.trim()}
          className="rounded-lg bg-signal px-3 py-1.5 text-xs font-semibold text-signal-fg transition hover:opacity-90 disabled:opacity-50"
        >
          {busy ? "Launching…" : "Configure & run"}
        </button>
        <button
          type="button"
          onClick={() => setOpen(false)}
          disabled={busy}
          className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium text-foreground-muted hover:text-foreground"
        >
          Cancel
        </button>
        {error && <span role="alert" className="text-xs text-danger">{error}</span>}
      </div>
    </form>
  );
}
