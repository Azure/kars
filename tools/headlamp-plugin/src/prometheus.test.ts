// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { promQuery, usePromPoll } from "./prometheus";

const host = vi.hoisted(() => ({ cluster: "main" as string | null, request: vi.fn() }));
vi.mock("@kinvolk/headlamp-plugin/lib", () => ({
  ApiProxy: { request: host.request },
  K8s: { useCluster: () => host.cluster },
}));

beforeEach(() => { host.cluster = "main"; host.request.mockReset(); });
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.useRealTimers();
  delete (window as Window & { KARS_PROMETHEUS_URL?: string }).KARS_PROMETHEUS_URL;
});

function response(body: unknown, status = 200) {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status })));
}

describe("Prometheus response validation", () => {
  it("queries the existing service through the selected cluster's authenticated proxy", async () => {
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);
    host.request.mockResolvedValue(new Response(JSON.stringify({
      status: "success", data: { resultType: "vector", result: [] },
    })));
    expect(await promQuery({ cluster: "fresh" }, 'up{job="router"}')).toEqual([]);
    expect(host.request).toHaveBeenCalledWith(
      "/api/v1/namespaces/monitoring/services/kps-kube-prometheus-stack-prometheus:9090/proxy/api/v1/query?query=up%7Bjob%3D%22router%22%7D",
      { method: "GET", cluster: "fresh", isJSON: false, timeout: 10000 },
    );
    expect(fetch).not.toHaveBeenCalled();
  });

  it("preserves proxy authorization failures without falling back to localhost", async () => {
    host.request.mockRejectedValue(new Error("Forbidden (HTTP 403)"));
    vi.stubGlobal("fetch", vi.fn());
    await expect(promQuery({ cluster: "fresh" }, "up")).rejects.toThrow("403");
    expect(fetch).not.toHaveBeenCalled();
  });

  it("does not query an implicit cluster when none is selected", async () => {
    await expect(promQuery({ cluster: null }, "up")).rejects.toThrow("Select a cluster");
    expect(host.request).not.toHaveBeenCalled();
  });

  it("preserves an observed zero and query encoding", async () => {
    response({ status: "success", data: {
      resultType: "vector", result: [{ metric: { sandbox: "agent" }, value: [123, "0"] }],
    } });
    expect(await promQuery("http://localhost:19091", 'sum(foo{sandbox="agent"})'))
      .toEqual([{ metric: { sandbox: "agent" }, value: 0 }]);
    expect(fetch).toHaveBeenCalledWith(
      "http://localhost:19091/api/v1/query?query=sum(foo%7Bsandbox%3D%22agent%22%7D)",
      expect.objectContaining({ signal: expect.any(AbortSignal) }),
    );
    expect(host.request).not.toHaveBeenCalled();
  });

  it("preserves missing samples instead of synthesizing zero", async () => {
    response({ status: "success", data: { resultType: "vector", result: [] } });
    expect(await promQuery("http://localhost", "foo")).toEqual([]);
  });

  it("reports HTTP errors", async () => {
    response({}, 403);
    await expect(promQuery("http://localhost", "foo")).rejects.toThrow("HTTP 403");
  });

  it.each([
    null,
    {},
    { status: "error", error: "query failed" },
    { status: "success", data: {} },
    { status: "success", data: { resultType: "matrix", result: [] } },
  ])("rejects an unsuccessful or malformed response: %j", async body => {
    response(body);
    await expect(promQuery("http://localhost", "foo")).rejects.toThrow("instant-vector");
  });

  it.each(["NaN", "+Inf", "-Inf", "", "not-a-number"])("rejects invalid values: %s", async value => {
    response({ status: "success", data: {
      resultType: "vector", result: [{ metric: {}, value: [123, value] }],
    } });
    await expect(promQuery("http://localhost", "foo")).rejects.toThrow("non-finite sample");
  });

  it("rejects non-string labels", async () => {
    response({ status: "success", data: {
      resultType: "vector", result: [{ metric: { sandbox: 42 }, value: [123, "1"] }],
    } });
    await expect(promQuery("http://localhost", "foo")).rejects.toThrow("invalid metric label");
  });
});

describe("Prometheus polling availability", () => {
  it("uses the selected cluster unless an external URL is explicitly configured", async () => {
    const loader = vi.fn().mockResolvedValue(1);
    const { rerender } = renderHook(() => usePromPoll(0, loader));
    await waitFor(() => expect(loader).toHaveBeenCalledWith({ cluster: "main" }));
    (window as Window & { KARS_PROMETHEUS_URL?: string }).KARS_PROMETHEUS_URL = "https://metrics.example";
    rerender();
    await waitFor(() => expect(loader).toHaveBeenLastCalledWith("https://metrics.example"));
  });

  it("invalidates another cluster's data and discards its late responses", async () => {
    const pending: ((value: number) => void)[] = [];
    const loader = vi.fn(() => new Promise<number>(resolve => { pending.push(resolve); }));
    const { result, rerender } = renderHook(() => usePromPoll(0, loader));
    host.cluster = "fresh";
    rerender();
    expect(loader).toHaveBeenLastCalledWith({ cluster: "fresh" });
    await act(async () => { pending[1](7); });
    expect(result.current).toEqual({ data: 7, err: "", ready: true });
    await act(async () => { pending[0](99); });
    expect(result.current).toEqual({ data: 7, err: "", ready: true });
    host.cluster = "another";
    rerender();
    expect(result.current.ready).toBe(false);
  });

  it("does not present its initial value as an observation", () => {
    const { result } = renderHook(() => usePromPoll(0, () => new Promise<number>(() => {})));
    expect(result.current).toEqual({ data: 0, err: "", ready: false });
  });

  it("invalidates an earlier success on failure and recovers on the next poll", async () => {
    vi.useFakeTimers();
    const loader = vi.fn<() => Promise<number>>()
      .mockResolvedValueOnce(14)
      .mockRejectedValueOnce(new Error("connection refused"))
      .mockResolvedValueOnce(19);
    const { result } = renderHook(() => usePromPoll(0, loader));
    await act(async () => {});
    expect(result.current).toEqual({ data: 14, err: "", ready: true });
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(result.current.ready).toBe(false);
    expect(result.current.err).toContain("connection refused");
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(result.current).toEqual({ data: 19, err: "", ready: true });
  });

  it("does not reuse another sandbox's successful sample", async () => {
    const first = vi.fn().mockResolvedValue(7);
    const second = vi.fn(() => new Promise<number>(() => {}));
    const { result, rerender } = renderHook(
      ({ sandbox }) => usePromPoll(0, sandbox === "one" ? first : second, 5000, sandbox),
      { initialProps: { sandbox: "one" } },
    );
    await waitFor(() => expect(result.current.ready).toBe(true));
    rerender({ sandbox: "two" });
    expect(result.current.ready).toBe(false);
    expect(second).toHaveBeenCalledOnce();
  });

  it("allows a slow response to finish without overlapping or discarding it", async () => {
    vi.useFakeTimers();
    const loader = vi.fn(() => new Promise<number>(resolve => setTimeout(() => resolve(23), 7000)));
    const { result, unmount } = renderHook(() => usePromPoll(0, loader));
    await act(async () => { await vi.advanceTimersByTimeAsync(6000); });
    expect(loader).toHaveBeenCalledOnce();
    expect(result.current.ready).toBe(false);
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(result.current).toEqual({ data: 23, err: "", ready: true });
    unmount();
    await act(async () => { await vi.advanceTimersByTimeAsync(10000); });
    expect(loader).toHaveBeenCalledOnce();
  });
});
