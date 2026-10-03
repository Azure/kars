// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { InClusterKubernetes } from "./kubernetes-json.js";
import { missionProcessConfig } from "./mission-config.js";
import { MissionDispatchService } from "./mission-service.js";
import { missionHealthServer, serveMissions } from "./mission-server.js";

const shutdown = new AbortController();
const stop = () => shutdown.abort();
process.once("SIGTERM", stop);
process.once("SIGINT", stop);
try {
  const config = missionProcessConfig(process.env);
  const service = new MissionDispatchService(new InClusterKubernetes(), config.service);
  await serveMissions(service, missionHealthServer(service), shutdown.signal, config);
  process.exit(0);
} catch {
  console.error("[mission-dispatcher] process-failed");
  // Exit also retires a quarantined SDK/socket without releasing its writer to another process.
  process.exit(1);
}
