// kars Bridge Workspace — New mission (intake → editable package → launch).

import { IntakeFlow } from "./intake-flow";
import { getOptions, getEfficiency } from "@/lib/bff";
import type { Options, Efficiency } from "@/lib/types";

export const dynamic = "force-dynamic";

const EMPTY_OPTIONS: Options = {
  models: [],
  default_model: null,
  provider: null,
  runtimes: [],
  isolation: [],
  tool_policies: [],
  mcp_servers: [],
  mcp_profiles: [],
  memories: [],
  skills: [],
};

export default async function NewMissionPage({
  searchParams,
}: {
  searchParams: Promise<{ intent?: string }>;
}) {
  const { intent } = await searchParams;
  // The composable palette — real models/runtimes/policies/services/memory read
  // from the live cluster. Never fabricated; an unreachable BFF yields the empty
  // palette and the package degrades to the controller defaults honestly.
  let options: Options = EMPTY_OPTIONS;
  try {
    options = await getOptions();
  } catch {
    // Non-fatal: the package still renders with honest "uses default" states.
  }

  // The learned efficiency frontier — what actually performs best per outcome.
  // This is the spine of orchestration: the recommendation the composer surfaces
  // at the point of choice comes from real completed runs, not a static default.
  let efficiency: Efficiency | null = null;
  try {
    efficiency = await getEfficiency();
  } catch {
    efficiency = null;
  }

  return (
    <div className="mx-auto max-w-3xl space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Start a mission</h1>
        <p className="mt-1 text-sm text-foreground-muted">
          Describe what you want. Bridge composes a complete, editable package — the model and
          harness it runs on, its instructions, the tools and services it may use, where it may
          reach on the network, how much it can act on its own, and the limits it runs under — for
          you to review and adjust before anything starts.
        </p>
      </div>
      <IntakeFlow options={options} efficiency={efficiency} initialObjective={intent} />
    </div>
  );
}
