// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomUUID } from "node:crypto";
import { existsSync, readFileSync, statSync, unlinkSync, appendFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { withRootMigrationJournal } from "./private-root-migration-journal.js";
import { commitRootPublication, type RootMigrationReceipt } from "./private-root-migration.js";

describe("durable private migration journal", () => {
  it.each(["success", "failure", "lost-acknowledgement"])("journals publication ownership and %s conservatively", async outcome => {
    const file = `.migration-publication-test-${randomUUID()}.jsonl`;
    const receipt = { version: 1, phase: "rotation-required", publication: {
      phase: "prepared", context: "kind-chosen", release: "kars", namespace: "kars-system",
      revision: 2, digest: "a".repeat(64),
    } } as RootMigrationReceipt;
    let launched = 0;
    try {
      await withRootMigrationJournal(file, true, async journal => {
        await journal.save(receipt);
        const saving = { save: async (next: RootMigrationReceipt) => {
          if (outcome === "lost-acknowledgement" && next.publication?.phase === "succeeded") throw new Error("fsync failed");
          await journal.save(next);
        } };
        const publishing = commitRootPublication(receipt, saving, async () => {
          launched++;
          expect(readFileSync(file, "utf8")).toContain('"phase":"publishing"');
          if (outcome === "failure") throw new Error("Helm failed after resource writer started");
        });
        if (outcome === "success") await publishing;
        else await expect(publishing).rejects.toThrow();
      });
      await withRootMigrationJournal(file, false, async (journal, current) => {
        expect(current!.publication!.phase).toBe(outcome === "success" ? "succeeded" : "publishing");
        if (outcome !== "success") {
          await expect(commitRootPublication(current!, journal, async () => { launched++; }))
            .rejects.toThrow(/unknown|ambiguous/i);
        }
      });
      expect(launched).toBe(1);
    } finally {
      if (existsSync(file)) unlinkSync(file);
      if (existsSync(`${file}.lock`)) unlinkSync(`${file}.lock`);
    }
  });

  it("does not launch when the ownership record cannot be saved", async () => {
    let launched = false;
    await expect(commitRootPublication({ publication: { phase: "prepared" } } as RootMigrationReceipt,
      { save: async () => { throw new Error("fsync failed"); } }, async () => { launched = true; }))
      .rejects.toThrow("fsync failed");
    expect(launched).toBe(false);
  });

  it("persists private chained records before returning, refuses concurrent ownership, and reopens the last record", async () => {
    const file = `.migration-journal-test-${randomUUID()}.jsonl`;
    const receipt = { version: 1, phase: "prepared" } as RootMigrationReceipt;
    try {
      await withRootMigrationJournal(file, true, async journal => {
        await journal.save(receipt);
        expect(statSync(file).mode & 0o777).toBe(0o600);
        expect(readFileSync(file, "utf8")).toContain('"phase":"prepared"');
        await expect(withRootMigrationJournal(file, false, async () => {})).rejects.toThrow();
        await journal.save({ ...receipt, phase: "quiescent" });
      });
      await withRootMigrationJournal(file, false, async (_journal, current) => {
        expect(current?.phase).toBe("quiescent");
      });
      expect(existsSync(`${file}.lock`)).toBe(false);
      appendFileSync(file, '{"torn":');
      await expect(withRootMigrationJournal(file, false, async () => {})).rejects.toThrow("torn");
    } finally {
      if (existsSync(file)) unlinkSync(file);
      if (existsSync(`${file}.lock`)) unlinkSync(`${file}.lock`);
    }
  });
});
