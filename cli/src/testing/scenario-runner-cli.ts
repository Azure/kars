#!/usr/bin/env node
// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Scenario runner CLI — plan item T5.
 *
 * Usage:
 *   node cli/dist/testing/scenario-runner-cli.js path/to/scenario.yaml [more.yaml ...]
 *
 * Exit code is the number of scenarios that failed (0 = all green).
 */
import { readdir, stat } from "node:fs/promises";
import { join } from "node:path";
import { runScenarioFile } from "./scenario.js";

async function expand(inputs: string[]): Promise<string[]> {
  const out: string[] = [];
  for (const p of inputs) {
    const st = await stat(p);
    if (st.isDirectory()) {
      const entries = await readdir(p);
      for (const e of entries.sort()) {
        if (e.endsWith(".yaml") || e.endsWith(".yml")) out.push(join(p, e));
      }
    } else {
      out.push(p);
    }
  }
  return out;
}

async function main(): Promise<number> {
  const args = process.argv.slice(2);
  if (args.length === 0) {
    process.stderr.write(
      "usage: scenario-runner-cli <scenario.yaml|dir> [more ...]\n",
    );
    return 2;
  }
  const files = await expand(args);
  if (files.length === 0) {
    process.stderr.write("no .yaml scenarios found\n");
    return 2;
  }

  let failed = 0;
  for (const file of files) {
    try {
      const result = await runScenarioFile(file);
      if (result.ok) {
        process.stdout.write(`PASS  ${result.name}  (${file})\n`);
      } else {
        failed += 1;
        process.stdout.write(`FAIL  ${result.name}  (${file})\n`);
        for (const step of result.steps) {
          if (!step.ok) {
            process.stdout.write(`  - ${step.step} [HTTP ${step.status}]\n`);
            for (const f of step.failures) {
              process.stdout.write(`      ${f}\n`);
            }
          }
        }
      }
    } catch (err) {
      failed += 1;
      process.stdout.write(`ERROR ${file}: ${(err as Error).message}\n`);
    }
  }
  return Math.min(failed, 125);
}

void main().then((code) => {
  process.exitCode = code;
});
