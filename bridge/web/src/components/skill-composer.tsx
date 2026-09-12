"use client";

// kars Bridge — shared skill-package composer. A guided visual form (name,
// version, summary, recipe, bounding tool policy, per-file editor with a
// SKILL.md starter template) that both the Workspace (user submits, lands
// PENDING review) and the Operator Console (operator authors the same way —
// this is the ONE creation path; editing an existing skill's raw spec is a
// separate, deliberate JSON escape hatch via AuthorResource) render
// identically. Previously the console had a bare file-picker requiring a
// pre-authored skill.json — this makes both surfaces the same experience.

import { useState, useTransition } from "react";
import { Icon } from "@/components/icon";
import type { RefOption } from "@/lib/types";

export interface SkillComposerInput {
  name: string;
  display_name: string;
  version: string;
  summary: string;
  bounding_policy: string;
  recipe?: string;
  files: { name: string; content: string }[];
}

export type SkillComposerResult = { ok: true } | { ok: false; error: string };

function slugify(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 60);
}

export function SkillComposer({
  toolPolicies,
  submit,
}: {
  toolPolicies: RefOption[];
  submit: (input: SkillComposerInput) => Promise<SkillComposerResult>;
}) {
  const [open, setOpen] = useState(false);
  const [displayName, setDisplayName] = useState("");
  const [version, setVersion] = useState("1.0.0");
  const [summary, setSummary] = useState("");
  const [recipe, setRecipe] = useState("");
  const [boundingPolicy, setBoundingPolicy] = useState(toolPolicies[0]?.name ?? "kars-default");
  const [files, setFiles] = useState<{ name: string; content: string }[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [pending, start] = useTransition();

  const valid = displayName.trim().length >= 2 && summary.trim().length >= 8 && version.trim().length > 0 && boundingPolicy.trim().length > 0;

  function onSubmit() {
    setError(null);
    start(async () => {
      const res = await submit({
        name: slugify(displayName),
        display_name: displayName.trim(),
        version: version.trim(),
        summary: summary.trim(),
        bounding_policy: boundingPolicy,
        recipe: recipe.trim() || undefined,
        files: files.filter((f) => f.name.trim() && f.content.trim()),
      });
      if (res.ok) {
        setDone(true);
        setDisplayName("");
        setSummary("");
        setRecipe("");
        setFiles([]);
        setVersion("1.0.0");
        setTimeout(() => setDone(false), 4000);
        setOpen(false);
      } else {
        setError(res.error);
      }
    });
  }

  if (!open) {
    return (
      <div className="kb-card flex flex-wrap items-center justify-between gap-3 p-4">
        <div>
          <p className="text-sm font-medium">Upload a skill</p>
          <p className="text-xs text-foreground-muted">
            Propose a capability package. It goes to an operator to scan, review, and sign before it&apos;s usable.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {done && <span className="text-xs font-medium text-ok">Submitted — pending review</span>}
          <button
            type="button"
            onClick={() => setOpen(true)}
            className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg hover:opacity-90"
          >
            Upload a skill
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="kb-card space-y-4 p-5">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold">Upload a skill</h2>
        <button type="button" onClick={() => setOpen(false)} className="text-xs text-foreground-muted hover:text-foreground">
          Cancel
        </button>
      </div>

      <fieldset className="rounded-lg border border-border p-3">
        <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="bolt" size={13} /> Package details</legend>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Name">
            <input
              value={displayName}
              onChange={(e) => setDisplayName(e.target.value)}
              placeholder="e.g. Repo triage"
              className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-signal"
            />
            {displayName && <p className="mt-1 font-mono text-[10px] text-foreground-muted">id: {slugify(displayName) || "—"}</p>}
          </Field>
          <Field label="Version">
            <input
              value={version}
              onChange={(e) => setVersion(e.target.value)}
              placeholder="1.0.0"
              className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-sm outline-none focus:border-signal"
            />
          </Field>
        </div>
        <div className="mt-3">
          <Field label="Summary">
            <input
              value={summary}
              onChange={(e) => setSummary(e.target.value)}
              placeholder="What the skill does, in one or two plain sentences."
              className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-signal"
            />
          </Field>
        </div>
        <div className="mt-3">
          <Field label="Recipe — standing instructions (optional)">
            <textarea
              value={recipe}
              onChange={(e) => setRecipe(e.target.value)}
              rows={3}
              placeholder="How the agent should use this capability, e.g. Label issues by area; close duplicates; flag regressions."
              className="w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-signal"
            />
          </Field>
        </div>
      </fieldset>

      <fieldset className="rounded-lg border border-border p-3">
        <div className="flex items-center justify-between">
          <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="file" size={13} /> Package files — SKILL.md + scripts (optional)</legend>
          <div className="flex gap-2">
            {!files.some((f) => f.name === "SKILL.md") && (
              <button
                type="button"
                onClick={() =>
                  setFiles((prev) => [
                    {
                      name: "SKILL.md",
                      content:
                        "---\nname: " +
                        (slugify(displayName) || "my-skill") +
                        "\ndescription: One clear sentence on WHAT this does and WHEN to use it — the agent reads this to decide.\n---\n\n# " +
                        (displayName || "My skill") +
                        "\n\nHow to use this capability. Reference scripts by name, e.g. run `bash hello.sh`.\n",
                    },
                    ...prev,
                  ])
                }
                className="rounded-md border border-signal/40 bg-signal/10 px-2 py-1 text-[11px] font-medium text-signal hover:bg-signal/15"
              >
                + SKILL.md template
              </button>
            )}
            <button
              type="button"
              onClick={() => setFiles((prev) => [...prev, { name: "", content: "" }])}
              className="rounded-md border border-border px-2 py-1 text-[11px] font-medium text-foreground-muted hover:text-foreground"
            >
              + File
            </button>
          </div>
        </div>
        <p className="mt-1 text-[11px] text-foreground-muted">
          A real package the agent installs and runs. It must include a <span className="font-mono">SKILL.md</span> with a
          frontmatter <span className="font-mono">description</span> — that&apos;s how the agent discovers and decides to use it.
          Flat filenames only (no folders). Installed on OpenClaw sandboxes.
        </p>
        <div className="mt-2 space-y-2">
          {files.map((f, i) => (
            <div key={i} className="rounded-md border border-border bg-surface p-2">
              <div className="flex items-center gap-2">
                <input
                  value={f.name}
                  onChange={(e) => setFiles((prev) => prev.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)))}
                  placeholder="filename (e.g. SKILL.md, hello.sh)"
                  className="flex-1 rounded border border-border bg-surface px-2 py-1 font-mono text-xs outline-none focus:border-signal"
                />
                <button
                  type="button"
                  onClick={() => setFiles((prev) => prev.filter((_, j) => j !== i))}
                  className="shrink-0 text-foreground-muted hover:text-danger"
                >
                  <Icon name="cross" size={13} />
                </button>
              </div>
              <textarea
                value={f.content}
                onChange={(e) => setFiles((prev) => prev.map((x, j) => (j === i ? { ...x, content: e.target.value } : x)))}
                rows={4}
                placeholder="file content"
                className="mt-1 w-full resize-y rounded border border-border bg-surface px-2 py-1 font-mono text-[11px] outline-none focus:border-signal"
              />
            </div>
          ))}
        </div>
      </fieldset>

      <fieldset className="rounded-lg border border-border p-3">
        <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="shield" size={13} /> Bounding tool policy</legend>
        <select
          value={boundingPolicy}
          onChange={(e) => setBoundingPolicy(e.target.value)}
          className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-signal"
        >
          {(toolPolicies.length ? toolPolicies : [{ name: "kars-default", summary: null }]).map((p) => (
            <option key={p.name} value={p.name}>
              {p.name}
              {p.summary ? ` — ${p.summary}` : ""}
            </option>
          ))}
        </select>
        <p className="mt-1.5 text-[11px] text-foreground-muted">
          The cap on what this skill may do — chosen from the policies your operator vetted.
        </p>
      </fieldset>

      {error && <p className="rounded-lg border border-danger/30 bg-danger/[0.06] px-3 py-2 text-xs text-danger">{error}</p>}

      <div className="flex items-center gap-2">
        <button
          type="button"
          disabled={!valid || pending}
          onClick={onSubmit}
          className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg hover:opacity-90 disabled:opacity-50"
        >
          {pending ? "Submitting…" : "Submit for review"}
        </button>
        <p className="text-[11px] text-foreground-muted">Lands pending — an operator scans + signs before it&apos;s usable.</p>
      </div>
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block text-xs text-foreground-muted">
      {label}
      <div className="mt-1">{children}</div>
    </label>
  );
}
