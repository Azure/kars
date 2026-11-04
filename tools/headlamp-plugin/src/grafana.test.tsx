// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import * as React from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { GrafanaDashboard, grafanaBaseUrl } from "./grafana";

const state = vi.hoisted(() => ({ service: null as object | null, error: null as { status: number } | null }));
vi.mock("@kinvolk/headlamp-plugin/lib", () => ({
  K8s: { ResourceClasses: { Service: { useGet: (name: string, namespace: string) => {
    expect([name, namespace]).toEqual(["kps-grafana", "monitoring"]);
    return [state.service, state.error];
  } } } },
}));
afterEach(() => { cleanup(); vi.unstubAllGlobals(); state.service = null; state.error = null; });

it.each(["localhost", "127.0.0.1", "[::1]"])("retains the documented CLI forward for local Headlamp at %s", host => {
  expect(grafanaBaseUrl(undefined, host)).toBe("http://127.0.0.1:3000");
});
it("never assumes a loopback dashboard for remote Headlamp", () => {
  expect(grafanaBaseUrl(undefined, "headlamp.example.org")).toBeNull();
});
it.each(["javascript:alert(1)", "data:text/html,test", "//example.org", "not a URL", "https://user:password@example.org", "https://example.org?token=secret", 123])("rejects invalid endpoint %s", value => {
  expect(grafanaBaseUrl(value, "localhost")).toBeNull();
});
it("preserves an explicitly configured endpoint's subpath", () => {
  expect(grafanaBaseUrl("https://ops.example.org/grafana/", "headlamp.example.org"))
    .toBe("https://ops.example.org/grafana");
});
it("does not link a dashboard while installation discovery is loading", () => {
  render(<GrafanaDashboard sandboxName="sre" />);
  expect(screen.getByRole("status").textContent).toContain("Loading Grafana discovery");
  expect(screen.queryByRole("link")).toBeNull();
});
it("reports absent optional Grafana without sending the browser to localhost", () => {
  state.error = { status: 404 };
  render(<GrafanaDashboard sandboxName="sre" />);
  expect(screen.getByRole("status").textContent).toContain("Grafana is not installed");
  expect(screen.queryByRole("link")).toBeNull();
});
it.each([403, 500])("does not mistake a %s discovery error for absent Grafana", status => {
  state.error = { status };
  render(<GrafanaDashboard sandboxName="sre" />);
  expect(screen.getByRole("status").textContent).toContain("discovery unavailable");
  expect(screen.queryByText(/not installed/)).toBeNull();
  expect(screen.queryByRole("link")).toBeNull();
});
it("uses an explicit external endpoint without requiring a cluster-local Grafana", () => {
  vi.stubGlobal("KARS_GRAFANA_URL", "https://ops.example.org/grafana/");
  state.error = { status: 404 };
  const { container } = render(<GrafanaDashboard sandboxName="one&two" />);
  const link = screen.getByRole("link");
  const url = new URL(link.getAttribute("href")!);
  expect(url.origin).toBe("https://ops.example.org");
  expect(url.pathname).toBe("/grafana/d/kars-ops");
  expect(url.searchParams.get("var-sandbox")).toBe("one&two");
  expect(link.getAttribute("target")).toBe("_blank");
  expect(link.getAttribute("rel")).toBe("noopener noreferrer");
  expect(container.querySelector("iframe")).toBeNull();
});
it.each(["sre", undefined])("links installed local Grafana without framing its authenticated session (%s)", sandboxName => {
  state.service = {};
  const { container } = render(<GrafanaDashboard sandboxName={sandboxName} />);
  expect(screen.getByText(/Local dashboard access requires/)).toBeTruthy();
  expect(screen.getByText(/Grafana’s own authenticated session/)).toBeTruthy();
  const link = screen.getByRole("link");
  const url = new URL(link.getAttribute("href")!);
  expect(url.origin).toBe("http://127.0.0.1:3000");
  expect(url.pathname).toBe("/d/kars-ops");
  expect(url.searchParams.get("var-sandbox")).toBe(sandboxName ?? null);
  expect(link.getAttribute("target")).toBe("_blank");
  expect(link.getAttribute("rel")).toBe("noopener noreferrer");
  expect(container.querySelector("iframe")).toBeNull();
});
