// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createServer, type Server } from "node:http";

interface MissionService {
  ready(): boolean;
  run(signal: AbortSignal): Promise<void>;
}

export function missionHealthServer(service: Pick<MissionService, "ready">): Server {
  const server = createServer((request, response) => {
    const path = request.url;
    response.setHeader("Cache-Control", "no-store");
    response.setHeader("Content-Type", "text/plain");
    if (request.method !== "GET") { response.writeHead(405).end(); return; }
    if (path === "/livez") { response.writeHead(200).end("live\n"); return; }
    if (path === "/readyz") {
      const ready = service.ready();
      response.writeHead(ready ? 200 : 503).end(ready ? "ready\n" : "not ready\n");
      return;
    }
    response.writeHead(404).end();
  });
  server.requestTimeout = 5_000;
  server.headersTimeout = 5_000;
  server.maxHeadersCount = 20;
  return server;
}

export async function serveMissions(
  service: MissionService,
  server: Server,
  signal: AbortSignal,
  options: { port: number; shutdownMs: number },
  forceExit: () => void = () => process.exit(1),
): Promise<void> {
  if (signal.aborted) return;
  let deadline: ReturnType<typeof setTimeout> | undefined;
  const boundShutdown = () => {
    deadline ??= setTimeout(forceExit, options.shutdownMs);
  };
  signal.addEventListener("abort", boundShutdown, { once: true });
  try {
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(options.port, "0.0.0.0", () => {
        server.removeListener("error", reject);
        resolve();
      });
    });
    await service.run(signal);
  } finally {
    // A failed bootstrap or uncertain SDK retirement also has a bounded process lifetime.
    boundShutdown();
    try {
      await new Promise<void>((resolve, reject) => {
        server.close(error => error && (error as NodeJS.ErrnoException).code !== "ERR_SERVER_NOT_RUNNING" ? reject(error) : resolve());
        server.closeAllConnections();
      });
    } finally {
      signal.removeEventListener("abort", boundShutdown);
      clearTimeout(deadline);
    }
  }
}
