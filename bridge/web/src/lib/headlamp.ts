// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export function headlampLink(baseUrl: string, route: string): string {
  const url = new URL(baseUrl);
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("Headlamp URL must use HTTP or HTTPS");
  }
  const basePath = url.pathname.replace(/\/+$/, "");
  // Core-managed Headlamp names its in-cluster context "main".
  const clusterPath = /\/c\/[^/]+$/.test(basePath) ? basePath : `${basePath}/c/main`;
  url.pathname = `${clusterPath}/${route.replace(/^\/+/, "")}`;
  return url.toString();
}
