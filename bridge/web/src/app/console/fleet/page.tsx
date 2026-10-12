// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Operator Console — Sandboxes. Every sandbox (lead + spawned
// sub-agents), with phase, runtime, isolation, parent, and the conditions table
// for troubleshooting. Real reads from KarsSandbox. The list itself is a
// filterable client surface (search + phase); this shell does the fetch + KPIs.

import { PageHeader, Stat } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { getClusterCapacity, listSandboxes } from "@/lib/bff";
import { FleetList } from "./fleet-list";
import { MeshTopology } from "./mesh-topology";
import { CapacityDashboard } from "./capacity-dashboard";
import { LiveRefresh } from "@/components/live-refresh";

export const dynamic = "force-dynamic";

async function readFleetObservation() {
  const [sandboxResult, capacityResult] = await Promise.allSettled([
    listSandboxes(),
    getClusterCapacity(),
  ]);
  return { sandboxResult, capacityResult, observedAt: Date.now() };
}

export default async function SandboxesPage({
  searchParams,
}: {
  searchParams?: Promise<{ q?: string }>;
}) {
  const initialQuery = (await searchParams)?.q ?? "";
  const { sandboxResult, capacityResult, observedAt } = await readFleetObservation();
  const sandboxes = sandboxResult.status === "fulfilled" ? sandboxResult.value : [];
  const capacity = capacityResult.status === "fulfilled" ? capacityResult.value : null;
  const error = sandboxResult.status === "rejected";

  const running = sandboxes.filter((s) => s.phase === "Running" || s.phase === "Ready").length;
  const executing = sandboxes.filter((s) => s.phase === "Running" && s.executing).length;
  const degraded = sandboxes.filter((s) => s.phase === "Degraded" || s.phase === "Failed").length;
  const suspended = sandboxes.filter((s) => s.phase === "Suspended").length;
  const subAgents = sandboxes.filter((s) => s.parent).length;

  return (
    <div className="space-y-6">
      <LiveRefresh active intervalMs={5000} />
      <PageHeader
        eyebrow="Operator Console"
        title="Sandboxes"
        lead="Every sandbox record across all namespaces, including suspended retained evidence. Running and Executing now are the live-agent counts."
      />

      {!error && sandboxes.length > 0 && (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-6">
          <Stat label="Sandboxes" value={sandboxes.length} />
          <Stat label="Running" value={running} accent={running > 0} />
          <Stat label="Executing now" value={executing} accent={executing > 0} />
          <Stat label="Sub-agents" value={subAgents} />
          <Stat label="Suspended" value={suspended} />
          <Stat label="Degraded" value={degraded} />
        </div>
      )}
      {!error && suspended > 0 && (
        <p className="text-xs text-foreground-muted">
          {suspended} suspended sandbox record{suspended === 1 ? "" : "s"} retain audit and deliverable linkage but consume no agent pod capacity.
        </p>
      )}
      {!error && sandboxes.length > 0 && running > executing && (
        <p className="text-xs text-foreground-muted">
          {`${running - executing} of ${running} running sandbox${running - executing === 1 ? "" : "es"} ${running - executing === 1 ? "is" : "are"} idle — already delivered (or a standing sandbox with no task attached), not actively executing right now. `}
          The Workspace&rsquo;s &ldquo;Active agents&rdquo; page counts exactly this same signal.
        </p>
      )}

      {capacity && !error ? (
        <CapacityDashboard capacity={capacity} sandboxes={sandboxes} />
      ) : (
        <HonestState
          compact
          variant="not_wired"
          title="Cluster & agent utilization unavailable"
          detail={capacity
            ? "Sandbox inventory is unavailable, so resource usage cannot be correlated with agents. This panel retries automatically."
            : "The Bridge backend could not load utilization. Existing sandbox records are shown when available. This panel retries automatically."}
        />
      )}

      {error ? (
        <HonestState variant="not_wired" title="Cluster unreachable" detail="The Bridge backend can't reach the Kubernetes API right now." />
      ) : sandboxes.length === 0 ? (
        <HonestState variant="empty" title="No sandboxes" detail="The substrate is idle — no agent sandboxes are running." />
      ) : (
        <>
          <MeshTopology sandboxes={sandboxes} />
          <FleetList sandboxes={sandboxes} observedAt={observedAt} initialQuery={initialQuery} />
        </>
      )}
    </div>
  );
}
