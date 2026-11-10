// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import * as React from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ChannelDiscovery, InferencePolicyMetricsCard, RouterPolicyStatusPanel, SandboxBudgetCard, TokenBudgetOverview, ToolPolicyMetricsCard } from "./index";

vi.mock("@kinvolk/headlamp-plugin/lib", () => ({
  registerRoute: vi.fn(), registerSidebarEntry: vi.fn(),
  ApiProxy: { request: (path: string, options: RequestInit) => fetch(path, options) },
  K8s: { useCluster: () => "main", ResourceClasses: { Service: { useGet: () => [{}] } } },
}));
vi.mock("@kinvolk/headlamp-plugin/lib/lib/k8s/crd", () => ({
  makeCustomResourceClass: () => ({
    useList: () => [[{
      metadata: { name: "policy", namespace: "default" },
      jsonData: { spec: { tokenBudget: { dailyTokens: 1000, perRequestTokens: 100 } } },
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
  SimpleTable: ({ data, columns }: { data: any[]; columns: any[] }) => <table><tbody>
    {data.map((row, i) => <tr key={i}>{columns.map((column, j) => <td key={j}>{column.getter(row)}</td>)}</tr>)}
  </tbody></table>,
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
  render(<SandboxBudgetCard sandboxName="one" namespace="default" inferenceRefName="policy" />);
  expect((await screen.findByRole("alert")).textContent).toContain("Telemetry unavailable");
  expect(screen.queryByText("Remaining")).toBeNull();
  expect(screen.queryByText("0.0%")).toBeNull();
});

function observedTokens(value: string, labels: Record<string, string> = { sandbox: "one", direction: "input" }) {
  vi.stubGlobal("fetch", vi.fn().mockImplementation(async () => new Response(JSON.stringify({
    status: "success", data: { resultType: "vector", result: [
      { metric: labels, value: [123, value] },
    ] },
  }))));
}

it("renders observed zero without inferring effective limits or remaining budget", async () => {
  observedTokens("0");
  render(<SandboxBudgetCard sandboxName="one" namespace="default" inferenceRefName="policy" />);
  await screen.findByText("Consumed (24h)");
  expect(screen.getByText("Policy daily setting")).toBeTruthy();
  expect(screen.getByText("1.0K")).toBeTruthy();
  expect(screen.getAllByText("0")).toHaveLength(2);
  expect(screen.getByText("No samples")).toBeTruthy();
  expect(screen.getByText(/not settled accounting/)).toBeTruthy();
  expect(screen.queryByText("Remaining")).toBeNull();
  expect(screen.queryByText("0.0%")).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
});

it("does not mistake a same-named policy in another namespace for this sandbox's policy", async () => {
  observedTokens("12");
  render(<SandboxBudgetCard sandboxName="one" namespace="another" inferenceRefName="policy" />);
  await screen.findByText("Consumed (24h)");
  expect(screen.getAllByText("Unavailable")).toHaveLength(2);
  expect(screen.queryByText("1.0K")).toBeNull();
  expect(screen.queryByText("unlimited")).toBeNull();
});

it("does not infer unlimited accounting from a missing inference reference", async () => {
  observedTokens("12");
  render(<SandboxBudgetCard sandboxName="one" namespace="default" />);
  await screen.findByText("Consumed (24h)");
  expect(screen.getAllByText("Unavailable")).toHaveLength(2);
  expect(screen.queryByText(/no enforced budget|unlimited/)).toBeNull();
});

it("keeps missing fleet samples unknown and policy settings separate from effective accounting", async () => {
  observedTokens("1500", { sandbox: "one" });
  const sandboxes = ["one", "two"].map(name => ({
    metadata: { name, namespace: "default" }, jsonData: { spec: { inferenceRef: { name: "policy" } } },
  }));
  const policies = [{ metadata: { name: "policy", namespace: "default" },
    jsonData: { spec: { tokenBudget: { dailyTokens: 1000 } } } }];
  render(<TokenBudgetOverview sandboxes={sandboxes as any} inferencePolicies={policies as any} />);
  await screen.findByText("Observed consumed (24h)");
  expect(screen.getByText("1 / 2")).toBeTruthy();
  expect(screen.getAllByText("1.0K")).toHaveLength(2);
  expect(screen.getAllByText("1.5K")).toHaveLength(2);
  expect(screen.getByText("No samples")).toBeTruthy();
  expect(screen.queryByText("Sandboxes over budget")).toBeNull();
  expect(screen.queryByText("Fleet utilization")).toBeNull();
  expect(screen.queryByText("0")).toBeNull();
});

it.each([new Error("Forbidden"), new Error("API unavailable")])("does not turn a credential-read failure into absent channels", error => {
  render(<ChannelDiscovery error={error} loading={false} empty><p>No channels configured.</p></ChannelDiscovery>);
  expect(screen.getByRole("alert").textContent).toContain("Credential discovery unavailable");
  expect(screen.queryByText("No channels configured.")).toBeNull();
});

it("does not report absent channels while discovery is loading", () => {
  render(<ChannelDiscovery error={null} loading empty><p>No channels configured.</p></ChannelDiscovery>);
  expect(screen.getByRole("status").textContent).toContain("Loading channel credentials");
  expect(screen.queryByText("No channels configured.")).toBeNull();
});

it("retains known inline channels while reporting incomplete credential discovery", () => {
  render(<ChannelDiscovery error={new Error("Forbidden")} loading={false} empty={false}><p>telegram</p></ChannelDiscovery>);
  expect(screen.getByRole("alert").textContent).toContain("available configuration only");
  expect(screen.getByText("telegram")).toBeTruthy();
});

it("reports no channels only after a successful empty discovery", () => {
  render(<ChannelDiscovery error={null} loading={false} empty><p>No channels configured.</p></ChannelDiscovery>);
  expect(screen.getByText("No channels configured.")).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
});

function observedPolicyMetrics(latency: string | null, count: string | null = "12") {
  vi.stubGlobal("fetch", vi.fn(async (path: string) => {
    const query = new URL(path, "http://localhost").searchParams.get("query")!;
    const quantile = query.includes("histogram_quantile");
    if (quantile) expect(query).toMatch(/ < \+Inf$/);
    const value = quantile ? latency : count;
    return new Response(JSON.stringify({ status: "success", data: {
      resultType: "vector", result: value === null ? [] : [{
        metric: quantile ? {} : { model: "observed-model", direction: "input", sandbox: "sre", decision: "allow" },
        value: [123, value],
      }],
    } }));
  }));
}

it.each([[null, "No observations"], ["0", "0 ms"], ["2.5", "2500 ms"]])(
  "preserves inference counters and Grafana access with latency %s", async (value, display) => {
    observedPolicyMetrics(value);
    render(<InferencePolicyMetricsCard policyName="sre-inference" />);
    await screen.findByText(display!);
    expect(screen.getByText("Tokens by model (1h)")).toBeTruthy();
    expect(screen.getByRole("heading", { name: /Fleet Inference Metrics/ })).toBeTruthy();
    expect(screen.getByText(/Fleet-wide aggregates.*not filtered to this policy/)).toBeTruthy();
    expect(screen.getByText("observed-model")).toBeTruthy();
    expect(screen.getByRole("link").getAttribute("href")).toContain("/d/kars-ops?");
    expect(screen.queryByRole("alert")).toBeNull();
  }
);

it.each([[null, "No observations"], ["0", "0 µs"], ["0.002", "2000 µs"]])(
  "preserves evaluation counters with latency %s", async (value, display) => {
    observedPolicyMetrics(value);
    render(<ToolPolicyMetricsCard policyName="sre-tools" />);
    await screen.findByText(display!);
    expect(screen.getByText(/Total evals/).textContent).toContain("12");
    expect(screen.getByRole("heading", { name: /Fleet Policy Evaluations/ })).toBeTruthy();
    expect(screen.getByText(/Fleet-wide AGT evaluation counters.*not filtered to this policy/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  }
);

it.each([[null, "No samples"], ["0", "0"]])("does not fabricate an evaluation for counter %s", async (value, display) => {
  observedPolicyMetrics(null, value);
  render(<ToolPolicyMetricsCard policyName="sre-tools" />);
  await screen.findByText("No observations");
  expect(screen.getByText(/Total evals/).textContent).toContain(display!);
  expect(screen.getByText(/Total evals/).textContent).not.toContain(": 1");
});

it("keeps Grafana reachable when inference telemetry fails", async () => {
  vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("Telemetry offline")));
  render(<InferencePolicyMetricsCard policyName="sre-inference" />);
  expect(screen.getByRole("link")).toBeTruthy();
  await screen.findByRole("alert");
  expect(screen.getByRole("link").getAttribute("target")).toBe("_blank");
  expect(screen.queryByText("0 ms")).toBeNull();
});

it.each([
  ["True", "RouterEnforcing", "all 1 referencing sandbox router(s) confirmed agt-profile digest"],
  ["False", "AwaitingRouterEnforcement", "router has not confirmed the new digest"],
  ["False", "NoSandboxesReferencing", "no sandboxes reference this policy"],
])("renders the ToolPolicy controller report for %s/%s without inventing an echo", (status, reason, message) => {
  render(<RouterPolicyStatusPanel crd={{ plural: "toolpolicies" } as any} item={{ jsonData: { status: {
    agtProfileDigest: "sha256:compiled",
    conditions: [{ type: "Ready", status, reason, message }],
  } } } as any} />);
  expect(screen.getByText(message)).toBeTruthy();
  expect(screen.getByRole("row", { name: `Confirmation ${reason}` })).toBeTruthy();
  expect(screen.queryByText("Loaded digest")).toBeNull();
  expect(screen.queryByText("Echo")).toBeNull();
  expect(screen.queryByText("(awaiting)")).toBeNull();
  expect(screen.queryByText("✓ matches")).toBeNull();
});

it("keeps an absent ToolPolicy confirmation unknown", () => {
  render(<RouterPolicyStatusPanel crd={{ plural: "toolpolicies" } as any} item={{ jsonData: { status: {
    agtProfileDigest: "sha256:compiled",
  } } } as any} />);
  expect(screen.getByText("No confirmation reported")).toBeTruthy();
  expect(screen.queryByText("RouterEnforcing")).toBeNull();
});

it.each([
  ["inferencepolicies", undefined, "(awaiting)"],
  ["inferencepolicies", "sha256:compiled", "✓ matches"],
  ["inferencepolicies", "sha256:different", "≠ mismatched"],
  ["karsmemories", undefined, "(awaiting)"],
  ["karsmemories", "sha256:compiled", "✓ matches"],
  ["karsmemories", "sha256:different", "≠ mismatched"],
])("preserves %s loaded digest %s comparison", (plural, loadedDigest, echo) => {
  render(<RouterPolicyStatusPanel crd={{ plural } as any} item={{ jsonData: { status: {
    compiledDigest: "sha256:compiled", loadedDigest,
  } } } as any} />);
  expect(screen.getByText("Loaded digest")).toBeTruthy();
  expect(screen.getByText(echo!)).toBeTruthy();
  expect(screen.queryByText("Controller report")).toBeNull();
});
