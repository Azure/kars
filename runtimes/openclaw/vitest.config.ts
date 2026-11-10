// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { defineConfig } from "vitest/config";
import { fileURLToPath } from "node:url";

export default defineConfig({
  resolve: { alias: {
    "@kars/mesh/dist/mission-protocol.js": fileURLToPath(new URL("../../mesh-plugin/src/mission-protocol.ts", import.meta.url)),
    "@kars/mesh/dist/mission-phase.js": fileURLToPath(new URL("../../mesh-plugin/src/mission-phase.ts", import.meta.url)),
  } },
  test: {
    include: ["src/**/*.test.ts"],
  },
});
