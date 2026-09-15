// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { artifactRoleAttribution } from "@/lib/team-run-evidence";
import type { MissionArtifact, TeamRole } from "@/lib/types";

export function ArtifactSummary({
  artifact,
  role,
}: {
  artifact: MissionArtifact;
  role?: TeamRole;
}) {
  const attribution = role
    ? artifactRoleAttribution(role, artifact) === "recorded"
      ? `from ${role.name.replace(/-/g, " ")} · recorded producer`
      : `possibly ${role.name.replace(/-/g, " ")} · inferred attribution`
    : artifact.source_agent?.trim()
      ? `recorded producer: ${artifact.source_agent} · not attributed to a roster role`
      : "producer unknown";

  return (
    <summary className="cursor-pointer px-4 py-3">
      <span className="font-medium">{artifact.name}</span>
      <span className="ml-2 text-xs text-foreground-muted">
        {attribution}
      </span>
    </summary>
  );
}
