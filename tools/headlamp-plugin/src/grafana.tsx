// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import * as React from "react";
import { K8s } from "@kinvolk/headlamp-plugin/lib";
import { useTheme } from "@mui/material/styles";

export function grafanaBaseUrl(override: unknown, hostname: string): string | null {
  if (override !== undefined && override !== "") {
    if (typeof override !== "string") return null;
    try {
      const url = new URL(override);
      if (!["http:", "https:"].includes(url.protocol) || url.username || url.password
        || url.search || url.hash) return null;
      return url.href.replace(/\/$/, "");
    } catch { return null; }
  }
  return ["localhost", "127.0.0.1", "[::1]"].includes(hostname)
    ? "http://127.0.0.1:3000" : null;
}

export function GrafanaDashboard({ sandboxName }: { sandboxName?: string }) {
  const theme = useTheme();
  const [service, error] = K8s.ResourceClasses.Service.useGet("kps-grafana", "monitoring");
  const override = (window as Window & { KARS_GRAFANA_URL?: string }).KARS_GRAFANA_URL;
  const base = grafanaBaseUrl(override, window.location.hostname);
  if (!override) {
    if (error) return <p role="status">{error.status === 404
      ? "Grafana is not installed in the selected cluster. Enable monitoring.grafana.enabled in the owning Core release to use dashboards."
      : "Grafana discovery unavailable. Check your Kubernetes Service-read permissions and connection."}</p>;
    if (!service) return <p role="status">Loading Grafana discovery…</p>;
  }
  if (!base) return <p role="status">
    Configure window.KARS_GRAFANA_URL with an HTTP(S) Grafana base URL reachable from this browser.
    Remote Headlamp does not assume a local port-forward.
  </p>;
  const url = `${base}/d/kars-ops?${new URLSearchParams({
    "kiosk": "tv", "refresh": "10s", "theme": theme.palette.mode,
    ...(sandboxName ? { "var-sandbox": sandboxName } : {}),
  })}`;
  return <>
    {!override && <p>Local dashboard access requires the Grafana port-forward started by <code>kars headlamp</code> for the selected cluster.</p>}
    <div style={{ marginBottom: 8 }}>
      <a href={url} target="_blank" rel="noopener noreferrer">Open full dashboard in Grafana ↗</a>
    </div>
    <p>Dashboards open in a separate tab using Grafana’s own authenticated session. The sandbox filter is preserved.</p>
  </>;
}
