// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * YAML scenario runner — plan item T5.
 *
 * Exercises the FakeRouter from declarative YAML files so behavior can be
 * validated without rebuilding the sandbox image or driving an LLM. Each
 * scenario describes:
 *
 *   - router.routes        → fixture table loaded into the FakeRouter
 *   - steps[*].request     → raw HTTP request fired at the fake router
 *   - steps[*].expect      → assertions on status / body / header
 *   - steps[*].expect.log  → assertions on the recorded request log
 *
 * This keeps the loop:
 *     edit scenario → node scenario-runner … → see pass/fail
 *
 * …under a second, versus the "build sandbox image, push to ACR, wait for
 * pod restart, talk to an LLM, read logs" loop currently required.
 *
 * Zero net-new runtime deps — we already have `yaml` in package.json for
 * Helm value manipulation, and `node:http` is stdlib.
 */
import { readFile } from "node:fs/promises";
import { resolve as resolvePath, dirname } from "node:path";
import * as YAML from "yaml";

import { FakeRouter, type FixtureRoute } from "./fake-router.js";

export interface ScenarioFile {
  name: string;
  description?: string;
  router?: {
    fixtures_dir?: string;
    routes?: FixtureRoute[];
  };
  steps: ScenarioStep[];
}

export interface ScenarioStep {
  name: string;
  request: {
    method: string;
    path: string;
    headers?: Record<string, string>;
    body?: unknown;
  };
  expect?: {
    status?: number;
    body_contains?: string | string[];
    body_json_has?: Record<string, unknown>;
    header?: Record<string, string>;
    log?: {
      total_requests?: number;
      last_method?: string;
      last_path?: string;
    };
  };
}

export interface StepResult {
  step: string;
  ok: boolean;
  failures: string[];
  status: number;
  body: string;
}

export interface ScenarioResult {
  name: string;
  ok: boolean;
  steps: StepResult[];
}

export async function loadScenario(path: string): Promise<ScenarioFile> {
  const raw = await readFile(path, "utf8");
  const parsed = YAML.parse(raw) as ScenarioFile;
  if (parsed == null || typeof parsed !== "object") {
    throw new Error(`scenario ${path}: expected a YAML mapping at root`);
  }
  if (typeof parsed.name !== "string" || parsed.name.length === 0) {
    throw new Error(`scenario ${path}: missing required field 'name'`);
  }
  if (!Array.isArray(parsed.steps) || parsed.steps.length === 0) {
    throw new Error(`scenario ${path}: 'steps' must be a non-empty array`);
  }
  return parsed;
}

export async function runScenario(
  scenario: ScenarioFile,
  baseDir: string,
): Promise<ScenarioResult> {
  const fixturesDir = scenario.router?.fixtures_dir
    ? resolvePath(baseDir, scenario.router.fixtures_dir)
    : undefined;
  const routes: FixtureRoute[] = scenario.router?.routes ?? [];

  const router = await FakeRouter.start({ routes, fixturesDir });
  const results: StepResult[] = [];
  try {
    for (const step of scenario.steps) {
      results.push(await runStep(router, step));
    }
  } finally {
    await router.stop();
  }
  return {
    name: scenario.name,
    ok: results.every((r) => r.ok),
    steps: results,
  };
}

async function runStep(
  router: FakeRouter,
  step: ScenarioStep,
): Promise<StepResult> {
  const url = new URL(step.request.path, router.baseUrl);
  const headers = new Headers(step.request.headers ?? {});
  let bodyStr: string | undefined;
  if (step.request.body !== undefined) {
    bodyStr =
      typeof step.request.body === "string"
        ? step.request.body
        : JSON.stringify(step.request.body);
    if (!headers.has("content-type"))
      headers.set("content-type", "application/json");
  }

  const resp = await fetch(url, {
    method: step.request.method.toUpperCase(),
    headers,
    body: bodyStr,
  });
  const body = await resp.text();
  const failures: string[] = [];

  const exp = step.expect ?? {};
  if (exp.status !== undefined && resp.status !== exp.status) {
    failures.push(`status: expected ${exp.status}, got ${resp.status}`);
  }
  if (exp.body_contains !== undefined) {
    const needles = Array.isArray(exp.body_contains)
      ? exp.body_contains
      : [exp.body_contains];
    for (const needle of needles) {
      if (!body.includes(needle)) {
        failures.push(`body_contains: missing '${needle}'`);
      }
    }
  }
  if (exp.body_json_has !== undefined) {
    try {
      const parsed = JSON.parse(body) as Record<string, unknown>;
      for (const [k, v] of Object.entries(exp.body_json_has)) {
        if (!deepEqual(parsed[k], v)) {
          failures.push(
            `body_json_has: ${k} expected ${JSON.stringify(v)}, got ${JSON.stringify(parsed[k])}`,
          );
        }
      }
    } catch {
      failures.push(`body_json_has: response body is not valid JSON`);
    }
  }
  if (exp.header !== undefined) {
    for (const [k, v] of Object.entries(exp.header)) {
      const got = resp.headers.get(k);
      if (got !== v) {
        failures.push(`header ${k}: expected '${v}', got '${got ?? "<missing>"}'`);
      }
    }
  }
  if (exp.log?.total_requests !== undefined) {
    if (router.log.length !== exp.log.total_requests) {
      failures.push(
        `log.total_requests: expected ${exp.log.total_requests}, got ${router.log.length}`,
      );
    }
  }
  if (exp.log?.last_method !== undefined) {
    const last = router.log[router.log.length - 1];
    if (!last || last.method.toUpperCase() !== exp.log.last_method.toUpperCase()) {
      failures.push(
        `log.last_method: expected '${exp.log.last_method}', got '${last?.method ?? "<none>"}'`,
      );
    }
  }
  if (exp.log?.last_path !== undefined) {
    const last = router.log[router.log.length - 1];
    if (!last || last.path !== exp.log.last_path) {
      failures.push(
        `log.last_path: expected '${exp.log.last_path}', got '${last?.path ?? "<none>"}'`,
      );
    }
  }

  return {
    step: step.name,
    ok: failures.length === 0,
    failures,
    status: resp.status,
    body,
  };
}

function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a == null || b == null) return false;
  if (typeof a !== "object" || typeof b !== "object") return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) return false;
    return a.every((v, i) => deepEqual(v, b[i]));
  }
  const ao = a as Record<string, unknown>;
  const bo = b as Record<string, unknown>;
  const keys = new Set([...Object.keys(ao), ...Object.keys(bo)]);
  for (const k of keys) {
    if (!deepEqual(ao[k], bo[k])) return false;
  }
  return true;
}

/** Convenience: load + run a YAML file. */
export async function runScenarioFile(path: string): Promise<ScenarioResult> {
  const scenario = await loadScenario(path);
  return await runScenario(scenario, dirname(path));
}
