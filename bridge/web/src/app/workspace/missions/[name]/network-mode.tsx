// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useEffect, useState } from "react";
import { Icon } from "@/components/icon";

type LearnedResp = {
  available: boolean;
  mode?: string;
  domains?: string[];
  enforced?: string[];
  reason?: string;
};

export function NetworkMode({
  ns,
  task,
  mode,
}: {
  ns: string;
  task: string;
  mode: string | null;
}) {
  const enforced = mode === "Strict";
  const [data, setData] = useState<LearnedResp | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let on = true;
    fetch(`/api/namespaces/${ns}/tasks/${task}/egress/learned`)
      .then((r) => {
        if (!r.ok) throw new Error(`Network evidence unavailable (${r.status})`);
        return r.json();
      })
      .then((d: LearnedResp) => {
        if (on) setData(d);
      })
      .catch(() => on && setData({ available: false }))
      .finally(() => on && setLoading(false));
    return () => {
      on = false;
    };
  }, [ns, task]);

  const learned = data?.domains ?? [];
  const enforcedList = data?.enforced ?? [];

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Network enforcement</h2>
          <p className="mt-0.5 max-w-xl text-xs text-foreground-muted">
            Missions use Strict egress. Governed model and connected-service access remain subject to their policies.
            New destinations require approved access during the run. Datapath observations do not grant access.
          </p>
        </div>
        <span
          className={`shrink-0 rounded-full border px-2.5 py-1 text-xs font-medium ${
            enforced
              ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-600"
              : "border-sky-500/30 bg-sky-500/10 text-sky-600"
          }`}
        >
          <span className="inline-flex items-center gap-1">
            <Icon name={enforced ? "lock" : "eye"} size={12} />
            {enforced ? "Reported: Strict" : mode ? `Unexpected mode: ${mode}` : "Mode unavailable"}
          </span>
        </span>
      </div>

      <div className="mt-4">
        <p className="text-[11px] uppercase tracking-wide text-foreground-muted">Reported baseline destinations</p>
        <p className="mt-1 text-xs text-foreground-muted">This list excludes temporary grants and does not confirm the router has installed a change.</p>
        {loading ? (
          <p className="mt-1 text-sm text-foreground-muted">Reading network evidence…</p>
        ) : (
          <>
            {data?.available === false && (
              <p className="mt-1 text-sm text-foreground-muted">Network evidence unavailable. Missing data does not prove no traffic or successful enforcement.</p>
            )}
            {enforcedList.length > 0 ? (
              <ul className="mt-2 flex flex-wrap gap-1.5">
                {enforcedList.map((h) => (
                  <li key={h} className="inline-flex items-center gap-1.5 rounded-full bg-surface-muted px-2.5 py-1 font-mono text-xs">
                    <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" aria-hidden />
                    {h}
                  </li>
                ))}
              </ul>
            ) : data?.available === true && (
              <p className="mt-1 text-sm text-foreground-muted">No baseline destinations reported. An empty baseline does not enable Learning.</p>
            )}
          </>
        )}
      </div>
      {learned.length > 0 && (
        <div className="mt-4">
          <p className="text-[11px] uppercase tracking-wide text-foreground-muted">Reported router observations</p>
          <p className="mt-1 text-xs text-foreground-muted">These observations do not establish whether a connection was allowed or denied.</p>
          <ul className="mt-2 space-y-1.5">
            {learned.map((host) => <li key={host} className="font-mono text-xs">{host}</li>)}
          </ul>
        </div>
      )}
    </section>
  );
}
