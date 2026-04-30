// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Scenario runner unit tests — plan item T5.
 *
 * These cover the YAML scenario engine itself (not a specific scenario):
 * parsing, fixture substitution, assertion logic, failure reporting. The
 * repo's three .yaml scenarios are exercised by a separate smoke test that
 * parses each and runs them against the FakeRouter.
 */
import { describe, it, expect } from "vitest";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import {
  loadScenario,
  runScenario,
  runScenarioFile,
  type ScenarioFile,
} from "./scenario.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const SCENARIOS_DIR = join(HERE, "scenarios");

describe("scenario runner", () => {
  it("parses a minimal inline scenario", async () => {
    const scenario: ScenarioFile = {
      name: "inline minimal",
      steps: [
        {
          name: "echo 200",
          request: { method: "GET", path: "/ping" },
          expect: { status: 200 },
        },
      ],
      router: {
        routes: [{ method: "GET", path: "/ping", body: { ok: true } }],
      },
    };
    const result = await runScenario(scenario, HERE);
    expect(result.ok).toBe(true);
    expect(result.steps[0]?.ok).toBe(true);
    expect(result.steps[0]?.status).toBe(200);
  });

  it("reports a clear failure when status differs", async () => {
    const scenario: ScenarioFile = {
      name: "wrong status",
      steps: [
        {
          name: "expect 404",
          request: { method: "GET", path: "/ping" },
          expect: { status: 404 },
        },
      ],
      router: {
        routes: [{ method: "GET", path: "/ping", body: { ok: true } }],
      },
    };
    const result = await runScenario(scenario, HERE);
    expect(result.ok).toBe(false);
    expect(result.steps[0]?.failures[0]).toMatch(/status: expected 404/);
  });

  it("asserts body_contains and log assertions", async () => {
    const scenario: ScenarioFile = {
      name: "body + log",
      steps: [
        {
          name: "post and inspect",
          request: { method: "POST", path: "/v1/chat", body: { q: "hi" } },
          expect: {
            body_contains: ["hello from fake"],
            log: {
              total_requests: 1,
              last_method: "POST",
              last_path: "/v1/chat",
            },
          },
        },
      ],
      router: {
        routes: [
          {
            method: "POST",
            path: "/v1/chat",
            body: { message: "hello from fake" },
          },
        ],
      },
    };
    const result = await runScenario(scenario, HERE);
    expect(result.ok).toBe(true);
  });

  it("body_json_has verifies top-level fields", async () => {
    const scenario: ScenarioFile = {
      name: "json shape",
      steps: [
        {
          name: "check fields",
          request: { method: "GET", path: "/v1/models" },
          expect: {
            body_json_has: { object: "list" },
          },
        },
      ],
      router: {
        routes: [
          { method: "GET", path: "/v1/models", body: { object: "list" } },
        ],
      },
    };
    const result = await runScenario(scenario, HERE);
    expect(result.ok).toBe(true);
  });

  it("loads the shipped YAML scenarios", async () => {
    const happy = await loadScenario(
      join(SCENARIOS_DIR, "01-chat-completion-happy-path.yaml"),
    );
    expect(happy.name).toMatch(/chat-completion/);
    expect(happy.steps.length).toBeGreaterThan(0);
  });

  it("runs 01-chat-completion-happy-path end-to-end", async () => {
    const result = await runScenarioFile(
      join(SCENARIOS_DIR, "01-chat-completion-happy-path.yaml"),
    );
    if (!result.ok) {
      const details = result.steps
        .filter((s) => !s.ok)
        .map(
          (s) =>
            `  ${s.step}: [status=${s.status}] ${s.failures.join("; ")}\n    body=${s.body.slice(0, 200)}`,
        )
        .join("\n");
      throw new Error(`scenario failed:\n${details}`);
    }
    expect(result.ok).toBe(true);
  });

  it("runs 02-content-filter-propagation end-to-end", async () => {
    const result = await runScenarioFile(
      join(SCENARIOS_DIR, "02-content-filter-propagation.yaml"),
    );
    expect(result.ok, JSON.stringify(result, null, 2)).toBe(true);
  });

  it("runs 03-rate-limit-passthrough end-to-end", async () => {
    const result = await runScenarioFile(
      join(SCENARIOS_DIR, "03-rate-limit-passthrough.yaml"),
    );
    expect(result.ok, JSON.stringify(result, null, 2)).toBe(true);
  });
});
