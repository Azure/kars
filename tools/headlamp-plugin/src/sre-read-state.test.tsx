// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import * as React from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SREChat, SREConsole } from "./index";

const reads = vi.hoisted(() => ({
  sandboxes: vi.fn<() => [object[] | null, Error | null]>(),
  actions: vi.fn<() => [object[] | null, Error | null]>(),
  deployments: vi.fn<() => [object[] | null, Error | null]>(),
}));

vi.mock("@kinvolk/headlamp-plugin/lib", () => ({
  registerRoute: vi.fn(), registerSidebarEntry: vi.fn(),
}));
vi.mock("@kinvolk/headlamp-plugin/lib/lib/k8s/crd", () => ({
  makeCustomResourceClass: ({ pluralName }: { pluralName: string }) => ({
    useList: pluralName === "karssandboxes" ? reads.sandboxes : reads.actions,
  }),
}));
vi.mock("@kinvolk/headlamp-plugin/lib/K8s/deployment", () => ({
  default: { useList: reads.deployments },
}));
vi.mock("@kinvolk/headlamp-plugin/lib/K8s/secret", () => ({ default: {} }));
vi.mock("@kinvolk/headlamp-plugin/lib/CommonComponents", () => ({
  SectionBox: ({ title, children }: React.PropsWithChildren<{ title: string }>) =>
    <section><h2>{title}</h2>{children}</section>,
  StatusLabel: ({ children }: React.PropsWithChildren) => <span>{children}</span>,
  Link: ({ children }: React.PropsWithChildren) => <span>{children}</span>,
  SimpleTable: () => <table />,
}));

beforeEach(() => {
  reads.sandboxes.mockReturnValue([null, null]);
  reads.actions.mockReturnValue([[], null]);
  reads.deployments.mockReturnValue([[], null]);
});
afterEach(cleanup);

it.each([SREConsole, SREChat])("does not mistake denied sandbox reads for absent SRE in %s", Component => {
  reads.sandboxes.mockReturnValue([[], new Error("Forbidden: cannot list karssandboxes")]);
  render(<Component />);
  expect(screen.getByRole("alert").textContent).toContain("Forbidden");
  expect(screen.queryByText(/is not deployed yet/)).toBeNull();
  expect(screen.queryByText("kars sre install")).toBeNull();
});

it("surfaces failed refreshes even when a prior sandbox list is retained", () => {
  reads.sandboxes.mockReturnValue([[
    { metadata: { name: "sre", namespace: "kars-system" } },
  ], new Error("API unavailable")]);
  render(<SREConsole />);
  expect(screen.getByRole("alert").textContent).toContain("API unavailable");
  expect(screen.queryByText(/cluster is quiet/)).toBeNull();
});

it("does not claim an empty approval queue when action reads fail", () => {
  reads.sandboxes.mockReturnValue([[{ metadata: { name: "sre", namespace: "kars-system" } }], null]);
  reads.actions.mockReturnValue([[], new Error("Forbidden: cannot list karssreactions")]);
  render(<SREConsole />);
  expect(screen.getByRole("alert").textContent).toContain("SRE actions");
  expect(screen.queryByText(/cluster is quiet/)).toBeNull();
});

it("reports unreadable workload health instead of a healthy zero", () => {
  reads.sandboxes.mockReturnValue([[{ metadata: { name: "sre", namespace: "kars-system" } }], null]);
  reads.deployments.mockReturnValue([[], new Error("Forbidden: cannot list deployments")]);
  render(<SREConsole />);
  expect(screen.getByRole("alert").textContent).toContain("workload health");
  expect(screen.queryByText("Healthy")).toBeNull();
});

it("keeps loading distinct from an observed absent installation", () => {
  const { rerender } = render(<SREConsole />);
  expect(screen.getByText("Loading cluster state…")).toBeTruthy();
  expect(screen.queryByText(/is not deployed yet/)).toBeNull();
  reads.sandboxes.mockReturnValue([[], null]);
  rerender(<SREConsole />);
  expect(screen.getByText(/is not deployed yet/)).toBeTruthy();
});

it("preserves the installed console for successful reads", () => {
  reads.sandboxes.mockReturnValue([[{ metadata: { name: "sre", namespace: "kars-system" } }], null]);
  render(<SREConsole />);
  expect(screen.getByText(/Cluster Health/)).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByText(/is not deployed yet/)).toBeNull();
});
