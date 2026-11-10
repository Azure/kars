// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { fileURLToPath } from "node:url";
import { mergeConfig } from "vite";
import { configDefaults } from "vitest/config";
import headlampConfig from "@kinvolk/headlamp-plugin/config/vite.config.mjs";

// The host supplies these virtual entry points in production.
const library = (path: string) => fileURLToPath(
  new URL(`./node_modules/@kinvolk/headlamp-plugin/lib/${path}`, import.meta.url),
);

export default mergeConfig(headlampConfig, {
  test: {
    // npm test runs this Node-only suite before the browser component tests.
    exclude: [...configDefaults.exclude, "patch-build-tool.test.mjs"],
  },
  resolve: {
    alias: {
      "@kinvolk/headlamp-plugin/lib/K8s/deployment": library("lib/k8s/deployment.js"),
      "@kinvolk/headlamp-plugin/lib/K8s/secret": library("lib/k8s/secret.js"),
      "@kinvolk/headlamp-plugin/lib/CommonComponents": library("components/common/index.js"),
    },
  },
});
