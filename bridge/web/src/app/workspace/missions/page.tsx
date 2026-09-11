// kars Bridge Workspace — Missions list. Plain-language projection of the task
// fleet, filterable by what the user cares about (running / ready / blocked).

import Link from "next/link";
import { HonestState } from "@/components/honest-state";
import { listTasks } from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import { type TaskSummary } from "@/lib/types";
import { MissionsList } from "./missions-list";

export const dynamic = "force-dynamic";

export default async function MissionsPage() {
  const ns = defaultNamespace();
  let tasks: TaskSummary[] = [];
  let error = false;
  try {
    tasks = (await listTasks(ns)).filter((t) => !t.team);
  } catch {
    error = true;
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Missions</h1>
          <p className="mt-1 text-sm text-foreground-muted">
            Everything you&apos;ve started. Open one to watch it, steer it, or review its work.
          </p>
        </div>
        <Link
          href="/workspace/new"
          className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg hover:opacity-90"
        >
          Start a mission
        </Link>
      </div>

      {error ? (
        <HonestState
          variant="not_wired"
          title="Missions are unavailable"
          detail="The run environment isn't reachable right now. Try again shortly."
        />
      ) : tasks.length === 0 ? (
        <HonestState
          variant="empty"
          title="No missions yet"
          detail="Use “Start a mission” above, or just describe an outcome from Home — Bridge composes the plan for you to review."
        />
      ) : (
        <MissionsList tasks={tasks} />
      )}
    </div>
  );
}
