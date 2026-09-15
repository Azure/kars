// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useState } from "react";
import {
  copyWitnessCommand, WITNESS_DISABLE_COMMAND, WITNESS_ENABLE_COMMAND,
} from "@/lib/datapath-witness";

export function DatapathSetup() {
  const [message, setMessage] = useState("");
  async function copy(action: "enable" | "disable") {
    try {
      setMessage(await copyWitnessCommand(action, navigator.clipboard));
    } catch {
      setMessage("Copy failed. Select the command below manually. No cluster change was made.");
    }
  }
  return (
    <details className="kb-card p-5">
      <summary className="cursor-pointer text-sm font-semibold">Operator setup / enable or disable</summary>
      <div className="mt-4 space-y-4 text-sm text-foreground-muted">
        <p>
          Optional and off by default. <strong className="text-foreground">A real cluster operator
          runs Helm outside Bridge.</strong> These buttons only copy commands; Bridge does not deploy,
          remove, or grant privileges to an observer.
        </p>
        <p>
          From the reviewed public Kars source, set <code>KARS_CONTEXT</code> explicitly and
          <code> WITNESS_VALUES</code> to a reviewed values file containing a built, published,
          pullable aggregator image digest and explicit sandbox names. This source does not
          promise a prepublished aggregator image. Linux nodes need readable kernel BTF and
          compatible eBPF support. The elevated IG DaemonSet uses a dedicated privileged-PSS namespace.
        </p>
        {(["enable", "disable"] as const).map((action) => (
          <div key={action}>
            <div className="mb-2 flex items-center justify-between gap-3">
              <p className="font-medium text-foreground">{action === "enable" ? "Request on" : "Request off"}</p>
              <button type="button" onClick={() => copy(action)}
                className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal">
                Copy {action} Helm command
              </button>
            </div>
            <pre className="overflow-x-auto rounded-lg border border-border bg-surface-muted p-3 font-mono text-xs">
              {action === "enable" ? WITNESS_ENABLE_COMMAND : WITNESS_DISABLE_COMMAND}
            </pre>
          </div>
        ))}
        <p role="status" aria-live="polite">{message}</p>
        <p>
          Preflight refuses detected legacy/shared IG or witness ownership conflicts; never use
          Helm adoption or force flags. Removal affects only this release&apos;s resources, not core
          Kars, models, or CNI. Off retains intent metadata, Helm history, and the dedicated namespace;
          inspect the operator&apos;s rollout and remaining pods before claiming capture has stopped.
        </p>
        <p>Build, permissions, limitations, and safe removal: <code>deploy/ebpf-witness/README.md</code>.</p>
      </div>
    </details>
  );
}
