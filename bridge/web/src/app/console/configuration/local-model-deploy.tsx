"use client";

// Deploy a local (in-cluster / AI Runway) model — used in the Model catalogue,
// where local models are managed. A real "add" flow: pick a curated model (or
// a free-text HuggingFace id), name it, deploy, then watch LIVE progress (a
// milestone percentage + the actual Kubernetes activity feed) until Running.

import { useEffect, useRef, useState, useTransition } from "react";
import { Icon } from "@/components/icon";
import { startLocalDeployAction, pollLocalDeploymentAction, type LocalDeployProgress } from "./local-inference-actions";
import type { LocalInferenceStatus, CuratedLocalModel } from "@/lib/bff";

/** Live progress for an in-flight local model deploy — a real percentage bar +
 *  the actual Kubernetes activity feed (image pull, scheduling, container
 *  start / fail), sourced from the BFF's live-status endpoint. */
export function LocalDeployTracker({
  name,
  progress,
  onDone,
}: {
  name: string;
  progress: LocalDeployProgress | null;
  onDone: () => void;
}) {
  const ready = !!progress?.ready;
  const failed = !!progress?.failed || !!progress?.error;
  const targetPct = ready ? 100 : (progress?.percent ?? 0);
  const [displayPct, setDisplayPct] = useState(0);
  useEffect(() => {
    let raf: number;
    const step = () => {
      setDisplayPct((cur) => {
        const diff = targetPct - cur;
        if (Math.abs(diff) < 0.5) return targetPct;
        raf = requestAnimationFrame(step);
        return cur + diff * 0.12;
      });
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
  }, [targetPct]);

  const activities = progress?.activities ?? [];
  const feedRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (feedRef.current) feedRef.current.scrollTop = feedRef.current.scrollHeight;
  }, [activities.length]);

  const barColor = failed ? "bg-danger" : "bg-signal";
  return (
    <fieldset className={`rounded-lg border p-3 ${failed ? "border-danger/40" : ready ? "border-signal/40 bg-signal/[0.04]" : "border-border"}`}>
      <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted">
        <Icon name={failed ? "warning" : ready ? "check" : "box"} size={13} />
        {failed ? "Deploy failed" : ready ? "Model ready" : "Deploying"} — <span className="font-mono">{name}</span>
      </legend>
      <div className="mb-1 flex items-center justify-between text-[11px]">
        <span className="text-foreground-muted">{failed ? (progress?.failureReason ?? "Failed") : ready ? "Running" : (progress?.phase ?? "Starting…")}</span>
        <span className="font-mono font-medium">{Math.round(displayPct)}%</span>
      </div>
      <div className="h-2 w-full overflow-hidden rounded-full bg-surface-muted">
        <div className={`h-full rounded-full transition-[width] duration-300 ${barColor} ${!ready && !failed ? "kb-shimmer" : ""}`} style={{ width: `${Math.max(3, displayPct)}%` }} />
      </div>
      {activities.length > 0 && (
        <div ref={feedRef} className="mt-2.5 max-h-32 space-y-1 overflow-y-auto rounded-lg border border-border bg-surface-muted/30 p-2">
          {activities.slice(-12).map((a, i) => {
            const warn = a.type === "Warning";
            return (
              <div key={`${a.reason}-${a.time}-${i}`} className="flex items-start gap-2 text-[11px] leading-snug">
                <span className={`mt-0.5 shrink-0 font-mono font-medium ${warn ? "text-warning" : "text-signal"}`}>{a.reason}</span>
                <span className={`min-w-0 ${warn ? "text-foreground" : "text-foreground-muted"}`}>{a.message}{a.count > 1 && <span className="ml-1 text-foreground-muted">×{a.count}</span>}</span>
              </div>
            );
          })}
        </div>
      )}
      {failed && (
        <div className="mt-2 space-y-2">
          {progress?.failureMessage && <p className="text-[11px] text-danger">{progress.failureMessage}</p>}
          {progress?.error && <p className="text-[11px] text-danger">{progress.error}</p>}
          <button type="button" onClick={onDone} className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted">Close & fix</button>
        </div>
      )}
      {ready && (
        <div className="mt-2.5 flex items-center gap-2">
          <p className="text-xs text-signal">Running — now in the catalogue below, tagged <span className="font-mono">local-{name}</span>.</p>
          <button type="button" onClick={onDone} className="rounded-lg border border-signal/40 bg-signal/10 px-3 py-1 text-xs font-medium text-signal hover:bg-signal/15">Done</button>
        </div>
      )}
    </fieldset>
  );
}

export function LocalModelDeploy({
  status,
  catalog,
  onClose,
}: {
  status: LocalInferenceStatus | null;
  catalog: CuratedLocalModel[];
  onClose: () => void;
}) {
  const [selected, setSelected] = useState<CuratedLocalModel | null>(null);
  const [advanced, setAdvanced] = useState(false);
  const [name, setName] = useState("");
  const [image, setImage] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [deployingName, setDeployingName] = useState<string | null>(null);
  const [progress, setProgress] = useState<LocalDeployProgress | null>(null);
  const [submitting, startSubmit] = useTransition();
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    if (!deployingName) return;
    let cancelled = false;
    const tick = async () => {
      const p = await pollLocalDeploymentAction(deployingName);
      if (cancelled) return;
      setProgress(p);
      if (p.ready || p.failed || p.error) {
        if (pollRef.current) clearInterval(pollRef.current);
        pollRef.current = null;
      }
    };
    void tick();
    pollRef.current = setInterval(() => void tick(), 3000);
    return () => {
      cancelled = true;
      if (pollRef.current) clearInterval(pollRef.current);
      pollRef.current = null;
    };
  }, [deployingName]);

  function submit() {
    setError(null);
    setProgress(null);
    startSubmit(async () => {
      const r = await startLocalDeployAction({ name, modelId: selected?.id ?? "", tier: selected?.tier ?? "cpu", image: image || undefined });
      if (r.error || !r.name) { setError(r.error ?? "deploy failed"); return; }
      setDeployingName(r.name);
    });
  }

  if (!status?.available) {
    return (
      <div className="rounded-lg border border-dashed border-border bg-surface-muted/30 p-3 text-xs text-foreground-muted">
        Local inference isn&rsquo;t set up on this cluster yet. Connect it via <span className="font-medium">＋ Connect a provider → Local (in-cluster)</span>, which walks an operator through the one-time AI Runway + KAITO install.
      </div>
    );
  }

  if (deployingName) {
    return <LocalDeployTracker name={deployingName} progress={progress} onDone={onClose} />;
  }

  return (
    <div className="space-y-3 rounded-lg border border-border bg-surface p-3">
      <fieldset>
        <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="brain" size={13} /> Choose a model to deploy</legend>
        <div className="mt-2 grid gap-2 sm:grid-cols-3">
          {catalog.map((m) => {
            const gpuBlocked = m.tier === "gpu" && !(status.gpu_node_count > 0);
            const sel = selected?.id === m.id;
            return (
              <button
                key={m.id}
                type="button"
                disabled={gpuBlocked}
                title={gpuBlocked ? "No GPU node detected on this cluster" : undefined}
                onClick={() => { setSelected(m); setAdvanced(false); setName(m.id.split("/").pop()!.replace(/[^a-z0-9-]/gi, "-").toLowerCase()); }}
                className={`flex items-start gap-2 rounded-lg border p-2.5 text-left transition disabled:cursor-not-allowed disabled:opacity-40 ${sel ? "border-signal bg-signal/[0.06]" : "border-border bg-surface hover:bg-surface-muted"}`}
              >
                <Icon name={m.tier === "cpu" ? "box" : "bolt"} size={15} className={sel ? "text-signal" : "text-foreground-muted"} />
                <span>
                  <span className="block text-sm font-medium">{m.label}</span>
                  <span className="block text-[11px] text-foreground-muted">{m.params} · {m.tier === "cpu" ? "runs on CPU" : gpuBlocked ? "needs a GPU node (none detected)" : "needs a GPU node"}</span>
                </span>
              </button>
            );
          })}
          <button
            type="button"
            onClick={() => { setAdvanced(true); setSelected(null); setName(""); }}
            className={`flex items-start gap-2 rounded-lg border p-2.5 text-left transition ${advanced ? "border-signal bg-signal/[0.06]" : "border-border bg-surface hover:bg-surface-muted"}`}
          >
            <Icon name="terminal" size={15} className={advanced ? "text-signal" : "text-foreground-muted"} />
            <span>
              <span className="block text-sm font-medium">Advanced: any HuggingFace model</span>
              <span className="block text-[11px] text-foreground-muted">CPU tier needs a pre-built AIKit image too.</span>
            </span>
          </button>
        </div>
      </fieldset>

      {(selected || advanced) && (
        <fieldset className="rounded-lg border border-border p-3">
          <legend className="px-1 text-xs font-medium text-foreground-muted">Deployment details</legend>
          <div className="grid gap-3 sm:grid-cols-2">
            <label className="block text-xs text-foreground-muted">
              Name
              <input value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. local-llama-1b" className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs" />
            </label>
            {advanced ? (
              <label className="block text-xs text-foreground-muted">
                Model id
                <input value={selected?.id ?? ""} onChange={(e) => setSelected({ id: e.target.value, label: e.target.value, tier: "cpu", params: "" })} placeholder="e.g. Qwen/Qwen2.5-0.5B-Instruct" className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs" />
              </label>
            ) : (
              <label className="block text-xs text-foreground-muted">
                Model id
                <input value={selected?.id ?? ""} readOnly className="mt-1 w-full rounded-lg border border-border bg-surface-muted/50 px-3 py-2 font-mono text-xs opacity-70" />
              </label>
            )}
          </div>
          {advanced && (
            <label className="mt-3 block text-xs text-foreground-muted">
              AIKit image (CPU tier only)
              <input value={image} onChange={(e) => setImage(e.target.value)} placeholder="ghcr.io/kaito-project/aikit/..." className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs" />
            </label>
          )}
        </fieldset>
      )}

      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={submit}
          disabled={submitting || (!selected && !advanced) || !name.trim() || !(selected?.id ?? "").trim()}
          className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50"
        >
          {submitting ? "Starting…" : "Deploy"}
        </button>
        <button type="button" onClick={onClose} className="text-xs text-foreground-muted hover:text-foreground">Cancel</button>
        {error && <p className="text-xs text-danger">{error}</p>}
      </div>
    </div>
  );
}
