// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { McpServer } from "@/lib/types";
import { mcpGuidance, mcpStatus } from "./mcp-status";

export function McpStatusDetails({ server }: { server: McpServer }) {
  const status = mcpStatus(server);
  const guidance = mcpGuidance(server);
  const diagnostic = (
    <p className="mt-1 break-words text-xs text-foreground-muted">
      {status.reason && <span className="font-medium">{status.reason}: </span>}
      {status.detail}
    </p>
  );

  return (
    <div className="mt-2">
      {guidance ? (
        <>
          <div role="status" className="space-y-1 text-xs">
            <p className="font-medium">{guidance.title}</p>
            <p className="text-foreground-muted">{guidance.detail}</p>
          </div>
          <details className="mt-2 text-xs text-foreground-muted">
            <summary className="cursor-pointer">Controller details</summary>
            {diagnostic}
          </details>
        </>
      ) : diagnostic}
    </div>
  );
}
