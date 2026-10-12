// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useState } from "react";
import {
  copyWitnessCommand, witnessCommands,
} from "@/lib/datapath-witness";
import type { DatapathWitness } from "@/lib/types/operations";

export function DatapathSetup({ ownerRelease }: { ownerRelease?: DatapathWitness["owner_release"] }) {
  const [message, setMessage] = useState("");
  const commands = witnessCommands(ownerRelease);
  async function copy(action: "enable" | "disable") {
    try {
      setMessage(await copyWitnessCommand(action, navigator.clipboard, ownerRelease));
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
          pullable AL3 runtime digest for both <code>aggregator.image</code> and
          <code> gadget.image</code>, plus explicit sandbox names. The maintained runtime includes
          both the client and daemon; this source does not promise a prepublished image.
          Linux nodes need readable kernel BTF and
          compatible eBPF support. The elevated IG DaemonSet uses a dedicated privileged-PSS namespace.
        </p>
        <p>
          {ownerRelease === "kars-datapath-witness"
            ? "The verified intent belongs to the standalone release. These commands preserve that owner."
            : "Use the Core chart: place witness values under datapathWitness. No third Helm release is needed. Review the entire Core upgrade before applying it."}
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
              {commands[action]}
            </pre>
          </div>
        ))}
        <p role="status" aria-live="polite">{message}</p>
        <p>
          Preflight refuses detected legacy/shared IG or witness ownership conflicts; never use
          Helm adoption or force flags. Disable the witness rather than uninstalling Core.
          Off retains intent metadata, Helm history, and the dedicated namespace;
          inspect the operator&apos;s rollout and remaining pods before claiming capture has stopped.
        </p>
        <p>Build, permissions, limitations, and safe removal: <code>deploy/ebpf-witness/README.md</code>.</p>
      </div>
    </details>
  );
}
