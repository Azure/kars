// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * `azureclaw operator --snapshot` — non-interactive panel render.
 *
 * Prints the same blessed-tag dashboard the live TUI produces, but as
 * a single snapshot to stdout (with blessed tags converted to plain
 * text). Useful for CI, the `kubectl claw attest <name>` read surface
 * (S11, future), and operators who want a one-shot view.
 *
 * Per S14 plan: TUI is read-only and per §0.2 #10 we never invent data;
 * any field we can't observe surfaces as `unknown` with a verbatim reason.
 */
import { KubectlDataSource, renderDashboard } from "./panels/index.js";

export interface SnapshotOpts {
  kubeContext?: string;
  panels?: string;
  perSandbox?: boolean;
}

/** Strip blessed-style `{...}` tag markers for plain-text snapshot output. */
export function stripBlessedTags(s: string): string {
  return s.replace(/\{\/?[^}]*\}/g, "");
}

export async function runSnapshot(opts: SnapshotOpts): Promise<void> {
  const ds = new KubectlDataSource(opts.kubeContext);
  const state = await ds.fetch();
  const out = renderDashboard(state, {
    panels: opts.panels,
    perSandbox: opts.perSandbox,
  });
  // eslint-disable-next-line no-console
  console.log(stripBlessedTags(out));
}
