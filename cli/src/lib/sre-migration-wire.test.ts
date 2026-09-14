// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import http from "node:http";
import { execa } from "execa";
import { describe, expect, it } from "vitest";
import { qualifyMigrationData } from "./sre-migration-data.js";
import { canonicalMigrationSchemas } from "./sre-migration.test-support.js";
import type { SchemaExecute } from "./schema-documents.js";

describe("actual kubectl migration raw transport (not Kubernetes validation)", () => {
  it("sends the UID/RV-bound stored Task unchanged through a dry-run PUT and reads raw JSON", async () => {
    const { before, after } = canonicalMigrationSchemas(true);
    const current = before.find(object => object.spec.names.kind === "KarsTask")!;
    const desired = after.find(object => object.spec.names.kind === "KarsTask")!;
    const object = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask",
      metadata: { name: "migration-contract", namespace: "kars-system", uid: "fixture-uid", resourceVersion: "19",
        creationTimestamp: "2026-09-12T00:00:00Z" },
      spec: { objective: "Inert migration data", envelope: { tier: 1, authorityCeiling: 1,
        budget: { tokens: 20, usdMicros: 0 }, delegationDepth: 0 }, execution: { launch: false } } };
    const requests: { method?: string; url: string; body: string; authorization?: string }[] = [];
    const server = http.createServer(async (request, response) => {
      let body = "";
      for await (const chunk of request) body += chunk;
      requests.push({ method: request.method, url: request.url!, body, authorization: request.headers.authorization });
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify(request.method === "GET" ? { metadata: {}, items: [object] } : object));
    });
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("Loopback fixture did not bind a TCP port");
    try {
      const execute: SchemaExecute = (file, args, options) => execa(file,
        [...args, "--server", `http://127.0.0.1:${address.port}`, "--kubeconfig=/dev/null"], options);
      const snapshot = await qualifyMigrationData(execute, current, desired);
      expect(snapshot.count).toBe(1);
      expect(requests.map(request => request.method)).toEqual(["GET", "PUT"]);
      const read = new URL(requests[0].url, "http://localhost");
      expect(read.pathname).toBe("/apis/kars.azure.com/v1alpha1/karstasks");
      expect(read.searchParams.get("limit")).toBe("513");
      const write = new URL(requests[1].url, "http://localhost");
      expect(write.pathname).toBe("/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karstasks/migration-contract");
      expect(write.searchParams.get("dryRun")).toBe("All");
      expect(JSON.parse(requests[1].body)).toEqual(object);
      expect(requests.every(request => request.authorization === undefined)).toBe(true);
    } finally {
      await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    }
  }, 30_000);
});
