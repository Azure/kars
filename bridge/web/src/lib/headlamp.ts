// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export function headlampLink(baseUrl: string, route: string, ssoEnabled = false): string {
  const url = new URL(baseUrl);
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("Headlamp URL must use HTTP or HTTPS");
  }
  const basePath = url.pathname.replace(/\/+$/, "");
  if (ssoEnabled) {
    const returnTo = route.startsWith("/") ? route : `/${route}`;
    if (!/^\/([A-Za-z0-9_-]+\/)*[A-Za-z0-9_-]*$/.test(returnTo)) {
      throw new Error("Headlamp SSO return route must be cluster-relative");
    }
    const cluster = basePath.match(/^(.*)\/c\/([^/]+)$/);
    url.pathname = `${cluster ? cluster[1] : basePath}/oidc`;
    url.search = "";
    url.hash = "";
    url.searchParams.set("cluster", cluster ? decodeURIComponent(cluster[2]) : "main");
    url.searchParams.set("return", returnTo);
    return url.toString();
  }
  // Core-managed Headlamp names its in-cluster context "main".
  const clusterPath = /\/c\/[^/]+$/.test(basePath) ? basePath : `${basePath}/c/main`;
  url.pathname = `${clusterPath}/${route.replace(/^\/+/, "")}`;
  return url.toString();
}
