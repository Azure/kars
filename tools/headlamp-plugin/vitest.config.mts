// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { fileURLToPath } from "node:url";
import { mergeConfig } from "vite";
import headlampConfig from "@kinvolk/headlamp-plugin/config/vite.config.mjs";

// The host supplies these virtual entry points in production.
const library = (path: string) => fileURLToPath(
  new URL(`./node_modules/@kinvolk/headlamp-plugin/lib/${path}`, import.meta.url),
);

export default mergeConfig(headlampConfig, {
  resolve: {
    alias: {
      "@kinvolk/headlamp-plugin/lib/K8s/deployment": library("lib/k8s/deployment.js"),
      "@kinvolk/headlamp-plugin/lib/K8s/secret": library("lib/k8s/secret.js"),
      "@kinvolk/headlamp-plugin/lib/CommonComponents": library("components/common/index.js"),
    },
  },
});
