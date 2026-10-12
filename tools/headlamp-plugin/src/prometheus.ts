// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { useEffect, useState } from "react";
import { ApiProxy, K8s } from "@kinvolk/headlamp-plugin/lib";

export interface PrometheusSample {
  metric: Record<string, string>;
  value: number;
}

type PrometheusSource = string | { cluster: string | null };
const PROMETHEUS_PROXY = "/api/v1/namespaces/monitoring/services/kps-kube-prometheus-stack-prometheus:9090/proxy";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export async function promQuery(source: PrometheusSource, query: string): Promise<PrometheusSample[]> {
  const suffix = `/api/v1/query?query=${encodeURIComponent(query)}`;
  let response: Response;
  if (typeof source === "string") {
    response = await fetch(`${source.replace(/\/$/, "")}${suffix}`, {
      signal: AbortSignal.timeout(10000),
    });
  } else {
    if (!source.cluster) throw new Error("Select a cluster to query Prometheus telemetry");
    response = await ApiProxy.request(`${PROMETHEUS_PROXY}${suffix}`, {
      method: "GET",
      cluster: source.cluster,
      isJSON: false,
      timeout: 10000,
    });
  }
  if (!response.ok) throw new Error(`Prometheus request failed (HTTP ${response.status})`);
  const body: unknown = await response.json();
  if (!isRecord(body) || body.status !== "success" || !isRecord(body.data)
    || body.data.resultType !== "vector" || !Array.isArray(body.data.result)) {
    throw new Error("Prometheus did not return a successful instant-vector response");
  }
  return body.data.result.map((row: unknown) => {
    if (!isRecord(row) || !isRecord(row.metric) || !Array.isArray(row.value)
      || row.value.length !== 2 || typeof row.value[0] !== "number"
      || !Number.isFinite(row.value[0]) || typeof row.value[1] !== "string"
      || row.value[1].trim() === "" || !Number.isFinite(Number(row.value[1]))) {
      throw new Error("Prometheus returned an invalid or non-finite sample");
    }
    const metric: Record<string, string> = {};
    for (const [key, value] of Object.entries(row.metric)) {
      if (typeof value !== "string") throw new Error("Prometheus returned an invalid metric label");
      metric[key] = value;
    }
    return { metric, value: Number(row.value[1]) };
  });
}

function prometheusOverride(): string | undefined {
  return (typeof window !== "undefined"
    && (window as Window & { KARS_PROMETHEUS_URL?: string }).KARS_PROMETHEUS_URL)
    || undefined;
}

export function usePromPoll<T>(
  initial: T,
  loader: (source: PrometheusSource) => Promise<T>,
  intervalMs = 5000,
  queryKey = "",
): { data: T; err: string; ready: boolean } {
  const cluster = K8s.useCluster();
  const base = prometheusOverride();
  const source = JSON.stringify([base, cluster, queryKey]);
  const [state, setState] = useState({ data: initial, err: "", source: "" });
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      try {
        const data = await loader(base || { cluster });
        if (!cancelled) setState({ data, err: "", source });
      } catch (error) {
        if (!cancelled) setState({ data: initial, err: String(error), source });
      }
      if (!cancelled) timer = setTimeout(poll, intervalMs);
    }
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
    // The query key identifies captured inputs; polling must not restart on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [base, cluster, queryKey, intervalMs]);
  const current = state.source === source;
  return { data: state.data, err: current ? state.err : "", ready: current && !state.err };
}
