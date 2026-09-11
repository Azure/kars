// kars Bridge Operator Console — Sandboxes. Every sandbox (lead + spawned
// sub-agents), with phase, runtime, isolation, parent, and the conditions table
// for troubleshooting. Real reads from KarsSandbox. The list itself is a
// filterable client surface (search + phase); this shell does the fetch + KPIs.

import { PageHeader, Stat } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { getClusterCapacity, listSandboxes } from "@/lib/bff";
import type { ClusterCapacity, Sandbox } from "@/lib/types";
import { FleetList } from "./fleet-list";
import { MeshTopology } from "./mesh-topology";
import { CapacityDashboard } from "./capacity-dashboard";
import { LiveRefresh } from "@/components/live-refresh";

export const dynamic = "force-dynamic";

export default async function SandboxesPage({
  searchParams,
}: {
  searchParams?: Promise<{ q?: string }>;
}) {
  const initialQuery = (await searchParams)?.q ?? "";
  let sandboxes: Sandbox[] = [];
  let capacity: ClusterCapacity | null = null;
  let error = false;
  try {
    [sandboxes, capacity] = await Promise.all([
      listSandboxes(),
      getClusterCapacity().catch(() => null),
    ]);
  } catch {
    error = true;
  }

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

      {capacity && <CapacityDashboard capacity={capacity} sandboxes={sandboxes} />}

      {error ? (
        <HonestState variant="not_wired" title="Cluster unreachable" detail="The Bridge backend can't reach the Kubernetes API right now." />
      ) : sandboxes.length === 0 ? (
        <HonestState variant="empty" title="No sandboxes" detail="The substrate is idle — no agent sandboxes are running." />
      ) : (
        <>
          <MeshTopology sandboxes={sandboxes} />
          <FleetList sandboxes={sandboxes} initialQuery={initialQuery} />
        </>
      )}
    </div>
  );
}
