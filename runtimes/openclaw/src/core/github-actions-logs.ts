// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import * as http from "node:http";
import { routerUrl } from "./router-client.js";

const MAX_LOG_BYTES = 2 * 1024 * 1024;
const DEFAULT_TAIL_LINES = 250;
const MAX_TAIL_LINES = 2_000;

export function normalizeGitHubJobLogRequest(
  owner: unknown,
  repo: unknown,
  jobId: unknown,
  tailLines: unknown,
): { owner: string; repo: string; jobId: string; tailLines: number } {
  const safe = (value: unknown, max: number): value is string =>
    typeof value === "string" && value.length <= max &&
    /^[A-Za-z0-9_.-]+$/.test(value) && value !== "." && value !== "..";
  if (!safe(owner, 39) || !safe(repo, 100)) {
    throw new Error("owner and repo must be safe GitHub path segments");
  }
  if (typeof jobId !== "string" || !/^[1-9][0-9]{0,19}$/.test(jobId) ||
      BigInt(jobId) > 18_446_744_073_709_551_615n) {
    throw new Error("job_id must be a positive numeric GitHub Actions job id string");
  }
  if (tailLines !== undefined && (typeof tailLines !== "number" || !Number.isFinite(tailLines))) {
    throw new Error("tail_lines must be a finite number");
  }
  return {
    owner, repo, jobId,
    tailLines: tailLines === undefined ? DEFAULT_TAIL_LINES :
      Math.min(Math.max(Math.trunc(tailLines as number), 1), MAX_TAIL_LINES),
  };
}

export function tailLogText(text: string, tailLines: number): string {
  return text.split(/\r?\n/).slice(-tailLines).join("\n");
}

export async function fetchGitHubActionsJobLogs(
  owner: unknown, repo: unknown, jobId: unknown, tailLines: unknown,
): Promise<string> {
  const request = normalizeGitHubJobLogRequest(owner, repo, jobId, tailLines);
  const url = new URL(routerUrl(
    `/gh-api/repos/${request.owner}/${request.repo}/actions/jobs/${request.jobId}/logs`,
  ));
  // This service is same-pod only; do not turn KARS_ROUTER_URL into a log/SSRF
  // escape hatch, nor send admin credentials or follow upstream redirects.
  if (url.protocol !== "http:" || !["127.0.0.1", "[::1]"].includes(url.hostname) ||
      url.username || url.password) {
    throw new Error("GitHub Actions logs require a loopback HTTP router");
  }
  const response = await new Promise<{
    status: number; body: string; truncated: boolean;
  }>((resolve, reject) => {
    let settled = false;
    let req: http.ClientRequest;
    const done = (error?: Error, result?: { status: number; body: string; truncated: boolean }) => {
      if (settled) return;
      settled = true;
      clearTimeout(deadline);
      if (error) reject(error);
      else resolve(result!);
    };
    const deadline = setTimeout(() => {
      done(new Error("GitHub Actions log request timed out"));
      req.destroy();
    }, 95_000);
    req = http.get(url, (res) => {
      const status = res.statusCode ?? 0;
      if (status < 200 || status >= 300) {
        // Do not include upstream error bodies, URLs or headers in tool output.
        done(new Error(`GitHub Actions job log request returned HTTP ${status}`));
        res.destroy();
        return;
      }
      const chunks: Buffer[] = [];
      let retained = 0;
      res.on("data", (chunk: Buffer) => {
        retained += chunk.length;
        if (retained > MAX_LOG_BYTES) {
          done(new Error("GitHub Actions log response exceeds the service limit"));
          res.destroy();
          req.destroy();
          return;
        }
        chunks.push(chunk);
      });
      res.on("aborted", () => done(new Error("GitHub Actions log response was interrupted")));
      res.on("error", () => done(new Error("GitHub Actions log response failed")));
      res.on("end", () => done(undefined, {
        status, body: Buffer.concat(chunks).toString("utf8"),
        truncated: res.headers["x-kars-log-truncated"] === "true",
      }));
    });
    req.on("error", () => done(new Error("GitHub Actions router request failed")));
  });
  return JSON.stringify({
    repository: `${request.owner}/${request.repo}`, job_id: request.jobId,
    http_status: response.status, tail_lines: request.tailLines,
    truncated_before_tail: response.truncated, log: tailLogText(response.body, request.tailLines),
  });
}
