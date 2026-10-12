// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { afterEach, expect, it, vi } from "vitest";
import { TaskSkillFiles, TASK_SKILL_ROOT, readTaskFile, writeTaskFile } from "./task-filesystem.js";

const fixture: Record<string, string> = JSON.parse(readFileSync(
  new URL("../../../../tests/fixtures/skill-package.json", import.meta.url), "utf8",
));
const expectedDigest = "sha256:ff733becaecde141e87b4b8223ca99091bb181295398a1ddace419976dd16615";
const digest = (raw: Uint8Array | string) => `sha256:${createHash("sha256").update(raw).digest("hex")}`;
const catalog = () => ({ schema: 1, sandboxUid: "sandbox-uid", skills: [{
  name: "checklist", uid: "skill-uid", generation: 2, versionDigest: `sha256:${"a".repeat(32)}`,
  summary: "Review local evidence", recipe: null, packageDigest: expectedDigest, files: { ...fixture },
}] });
const load = (value: unknown) => {
  const raw = Buffer.from(JSON.stringify(value));
  return new TaskSkillFiles(raw, digest(raw));
};
afterEach(() => vi.unstubAllEnvs());

it("reads the same canonical Unicode, numeric-name and empty-file package as Rust and Hermes", async () => {
  const skills = load(catalog());
  for (const [path, content] of Object.entries(fixture)) {
    const result = await readTaskFile(`${TASK_SKILL_ROOT}/checklist/${path}`, 65536, undefined, skills);
    expect(result.content).toBe(content);
    expect(result.bytes).toBe(Buffer.byteLength(content));
    expect(result.truncated).toBe(false);
  }
  expect(skills.instructions()).toContain("Read each selected package's SKILL.md");
  expect(skills.instructions()).toContain("does not grant shell");
});

it("does not discover ambient files without an explicit controller digest", () => {
  vi.stubEnv("KARS_SKILL_CATALOG_DIGEST", undefined);
  expect(TaskSkillFiles.load()).toBeUndefined();
});

it.each(["../secret", "/absolute", "a//b", "a/./b", "a__b", "a/../b", "x/".repeat(85) + "z"])(
  "rejects invalid source path %s before using package bytes", path => {
    const value = catalog();
    value.skills[0].files[path] = "content";
    expect(() => load(value)).toThrow("Invalid approved skill file");
  },
);

it("rejects parent files, missing manifests, duplicate identities and changed package bytes", () => {
  let value = catalog();
  value.skills[0].files.references = "not a directory";
  expect(() => load(value)).toThrow("parent file");
  value = catalog();
  delete value.skills[0].files["SKILL.md"];
  expect(() => load(value)).toThrow("package bytes");
  value = catalog();
  value.skills.push({ ...value.skills[0] });
  expect(() => load(value)).toThrow("identity");
  value = catalog();
  value.skills[0].files["SKILL.md"] += "Changed";
  expect(() => load(value)).toThrow("package bytes");
});

it("rejects wrong controller binding, oversized catalogs and invalid UTF-8", () => {
  const raw = Buffer.from(JSON.stringify(catalog()));
  expect(() => new TaskSkillFiles(raw, `sha256:${"0".repeat(64)}`)).toThrow("controller-bound");
  const large = Buffer.alloc(1024 * 1024 + 1, 32);
  expect(() => new TaskSkillFiles(large, digest(large))).toThrow("controller-bound");
  const bad = Buffer.from([0xff]);
  expect(() => new TaskSkillFiles(bad, digest(bad))).toThrow();
});

it("never falls through to disk or writes through reserved skill paths", async () => {
  const skills = load(catalog());
  for (const path of [
    `${TASK_SKILL_ROOT}/checklist/missing`, `${TASK_SKILL_ROOT}/other/SKILL.md`,
    `${TASK_SKILL_ROOT}/checklist/../SKILL.md`, `${TASK_SKILL_ROOT}/checklist/SKILL.md/`,
  ]) {
    await expect(readTaskFile(path, 1024, undefined, skills)).rejects.toThrow("manifest");
    await expect(writeTaskFile(path, "replace")).rejects.toThrow("read-only");
  }
  await expect(readTaskFile(`${TASK_SKILL_ROOT}/checklist/SKILL.md`, 1024)).rejects.toThrow("No approved");
});

it("preserves complete UTF-8 boundaries when truncating and keeps recipe-only skills fileless", () => {
  const skills = load(catalog());
  const path = `${TASK_SKILL_ROOT}/checklist/references/check.md`;
  expect(skills.read(path, 12)).toMatchObject({ content: "Consent: caf", returned_bytes: 12, truncated: true });
  expect(skills.read(path, 13)).toMatchObject({ content: "Consent: caf", returned_bytes: 12, truncated: true });
  expect(skills.read(path, 14)).toMatchObject({ content: "Consent: café", returned_bytes: 14, truncated: true });
  const recipe = { ...catalog().skills[0], files: {}, packageDigest: null, recipe: "Review the task as provided." };
  const recipeSkills = load({ ...catalog(), skills: [recipe] });
  expect(recipeSkills.hasFiles).toBe(false);
  expect(recipeSkills.instructions()).toContain(recipe.recipe);
});
