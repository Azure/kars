// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export function githubMcpRoutingError(
  rawUrl: unknown,
  method: unknown = "GET",
  configuredServers = process.env.KARS_MCP_SERVERS ?? "",
): string | null {
  const servers = configuredServers
    .split(",")
    .map((server) => server.trim().toLowerCase())
    .filter(Boolean);
  if (!servers.some((server) => server === "github" || server === "github-mcp")) {
    return null;
  }

  let url: URL;
  try {
    url = new URL(String(rawUrl ?? ""));
  } catch {
    return null;
  }
  const host = url.hostname.toLowerCase();
  const verb = String(method ?? "GET").toUpperCase();
  if (
    verb === "GET"
    && (host === "raw.githubusercontent.com" || host === "codeload.github.com")
  ) {
    return null;
  }
  if (
    host === "api.github.com"
    || host === "github.com"
    || host.endsWith(".github.com")
  ) {
    return "http_fetch blocked by capability routing: GitHub MCP is configured. "
      + "Use the GitHub MCP tool from the current tool catalog for repository, pull-request, "
      + "workflow, check, alert, issue, commit, or file facts. Only explicit GET downloads from "
      + "raw.githubusercontent.com or codeload.github.com may use http_fetch.";
  }
  return null;
}
