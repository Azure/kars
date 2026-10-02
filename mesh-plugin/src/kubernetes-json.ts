// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import * as https from "node:https";

export interface KubernetesJson {
  request<T>(method: string, path: string, body?: unknown, contentType?: string): Promise<T>;
}

export class KubernetesError extends Error {
  constructor(readonly status: number, readonly method: string, readonly path: string) {
    super(`Kubernetes ${method} ${path} returned HTTP ${status}`);
  }
}

function inClusterOrigin(): string {
  const host = process.env.KUBERNETES_SERVICE_HOST;
  if (!host) throw new Error("KUBERNETES_SERVICE_HOST is required in cluster");
  return `https://${host.includes(":") ? `[${host}]` : host}:${process.env.KUBERNETES_SERVICE_PORT_HTTPS || "443"}`;
}

/** In-cluster credentials are reread per request; redirects and insecure TLS are not supported. */
export class InClusterKubernetes implements KubernetesJson {
  private readonly endpoint: URL;
  constructor(
    endpoint = inClusterOrigin(),
    private readonly tokenPath = "/var/run/secrets/kubernetes.io/serviceaccount/token",
    private readonly caPath = "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt",
    private readonly timeoutMs = 15_000,
  ) {
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) throw new Error("Invalid Kubernetes request timeout");
    this.endpoint = new URL(endpoint);
    if (this.endpoint.protocol !== "https:" || this.endpoint.username || this.endpoint.password
      || this.endpoint.pathname !== "/" || this.endpoint.search || this.endpoint.hash) {
      throw new Error("Kubernetes endpoint must be an HTTPS origin");
    }
  }

  async request<T>(method: string, path: string, body?: unknown, contentType = "application/json"): Promise<T> {
    if (!path.startsWith("/") || path.startsWith("//") || /[\\\\#\u0000-\u0020\u007f]/.test(path)) throw new Error("Invalid Kubernetes path");
    const url = new URL(path, this.endpoint);
    if (url.origin !== this.endpoint.origin) throw new Error("Kubernetes path changed origin");
    const token = readFileSync(this.tokenPath, "utf8").trim();
    const ca = readFileSync(this.caPath);
    if (!token) throw new Error("Missing projected Kubernetes service account token");
    const data = body === undefined ? undefined : JSON.stringify(body);
    if (data && Buffer.byteLength(data) > 1024 * 1024) throw new Error("Kubernetes request exceeds 1 MiB");
    let deadline: ReturnType<typeof setTimeout> | undefined;
    return new Promise<T>((resolve, reject) => {
      const req = https.request(url, {
        method, ca, rejectUnauthorized: true,
        headers: { authorization: `Bearer ${token}`, accept: "application/json", "content-type": contentType },
      }, res => {
        const chunks: Buffer[] = [];
        let size = 0;
        res.on("data", chunk => {
          size += chunk.length;
          if (size > 2 * 1024 * 1024) { req.destroy(new Error("Kubernetes response exceeds 2 MiB")); return; }
          chunks.push(Buffer.from(chunk));
        });
        res.on("error", reject);
        res.on("aborted", () => reject(new Error("Kubernetes response aborted")));
        res.on("end", () => {
          clearTimeout(deadline);
          if ((res.statusCode ?? 0) < 200 || (res.statusCode ?? 0) >= 300) {
            reject(new KubernetesError(res.statusCode ?? 0, method, path));
            return;
          }
          if (res.statusCode === 204) { resolve(undefined as T); return; }
          try { resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")) as T); } catch (error) { reject(error); }
        });
      });
      deadline = setTimeout(() => req.destroy(new Error("Kubernetes request deadline exceeded")), this.timeoutMs);
      req.on("error", reject);
      req.end(data);
    }).finally(() => clearTimeout(deadline));
  }
}
