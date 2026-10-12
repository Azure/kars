// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import * as React from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { SandboxBudgetCard, TokenBudgetOverview } from "./index";

vi.mock("@kinvolk/headlamp-plugin/lib", () => ({
  registerRoute: vi.fn(), registerSidebarEntry: vi.fn(),
  ApiProxy: { request: (path: string, options: RequestInit) => fetch(path, options) },
  K8s: { useCluster: () => "main" },
}));
vi.mock("@kinvolk/headlamp-plugin/lib/lib/k8s/crd", () => ({
  makeCustomResourceClass: () => ({
    useList: () => [[{
      metadata: { name: "policy", namespace: "default" },
      spec: { tokenBudget: { dailyTokens: 1000, perRequestTokens: 100 } },
    }]],
  }),
}));
vi.mock("@kinvolk/headlamp-plugin/lib/K8s/deployment", () => ({ default: {} }));
vi.mock("@kinvolk/headlamp-plugin/lib/K8s/secret", () => ({ default: {} }));
vi.mock("@kinvolk/headlamp-plugin/lib/CommonComponents", () => ({
  SectionBox: ({ title, children }: React.PropsWithChildren<{ title: string }>) =>
    <section><h2>{title}</h2>{children}</section>,
  StatusLabel: ({ children }: React.PropsWithChildren) => <span>{children}</span>,
  Link: ({ children }: React.PropsWithChildren) => <span>{children}</span>,
  SimpleTable: () => <table />,
}));

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it("does not render fleet zero-consumption or utilization while loading", () => {
  vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(() => {})));
  render(<TokenBudgetOverview sandboxes={[]} inferencePolicies={[]} />);
  expect(screen.getByRole("status").textContent).toContain("Loading Prometheus");
  expect(screen.queryByText("Fleet consumed (24h)")).toBeNull();
  expect(screen.queryByText("0.0%")).toBeNull();
});

it("surfaces a failed fleet request instead of displaying a healthy zero", async () => {
  vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
  render(<TokenBudgetOverview sandboxes={[]} inferencePolicies={[]} />);
  expect((await screen.findByRole("alert")).textContent).toContain("Telemetry unavailable");
  expect(screen.queryByText("Fleet consumed (24h)")).toBeNull();
  expect(screen.queryByText("0.0%")).toBeNull();
});

it("treats an empty vector as unknown consumption", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(JSON.stringify({
    status: "success", data: { resultType: "vector", result: [] },
  }))));
  render(<TokenBudgetOverview sandboxes={[]} inferencePolicies={[]} />);
  await screen.findByText(/No Prometheus samples/);
  expect(screen.queryByText("Fleet consumed (24h)")).toBeNull();
});

it("surfaces sandbox failures instead of silently displaying full remaining budget", async () => {
  vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
  render(<SandboxBudgetCard sandboxName="one" inferenceRefName="policy" />);
  expect((await screen.findByRole("alert")).textContent).toContain("Telemetry unavailable");
  expect(screen.queryByText("Remaining")).toBeNull();
  expect(screen.queryByText("0.0%")).toBeNull();
});

it("renders actually observed zero consumption", async () => {
  vi.stubGlobal("fetch", vi.fn().mockImplementation(async () => new Response(JSON.stringify({
    status: "success", data: { resultType: "vector", result: [
      { metric: { sandbox: "one", direction: "input" }, value: [123, "0"] },
      { metric: { sandbox: "one", direction: "output" }, value: [123, "0"] },
    ] },
  }))));
  render(<SandboxBudgetCard sandboxName="one" inferenceRefName="policy" />);
  await screen.findByText("Consumed (24h)");
  expect(screen.getByText("Remaining")).toBeTruthy();
  expect(screen.getByText("0.0%")).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
});
