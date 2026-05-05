// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Operator-TUI panels overlay (S14) — wires the modular-panels view as
 * a blessed overlay box on the main dashboard. Extracted out of
 * `operator.ts` so the dashboard module stays under §4.2's 800-LOC cap.
 *
 * Drill-in: each CRD row is prefixed with `[N]` and the keys `1`–`9`
 * (and `0` → 10) open a detail popup with the verbose render of just
 * that single CRD instance. Mirrors the per-agent `i` drill-in dialog.
 */
import blessed from "blessed";
import {
  KubectlDataSource,
  renderDashboard,
  renderCrdSections,
  renderCrdItemDetail,
  type ClusterState,
  type CrdRow,
} from "./panels/index.js";

export interface PanelsOverlayCtx {
  screen: any;
  kubeContext?: string;
  panelOpts: { panels?: string; perSandbox?: boolean };
  dialogOpen(): boolean;
}

export interface PanelsOverlayHandle {
  /** True while the overlay is visible. */
  isOpen(): boolean;
  /** Hide if visible; no-op otherwise. */
  hide(): void;
  /** Toggle (show + fetch fresh state, or hide if already shown). */
  toggle(): Promise<void>;
}

export function createPanelsOverlay(ctx: PanelsOverlayCtx): PanelsOverlayHandle {
  const overlay = blessed.box({
    parent: ctx.screen,
    hidden: true,
    top: 1, left: 0, right: 0, bottom: 1,
    tags: true,
    scrollable: true,
    alwaysScroll: true,
    mouse: true,
    keys: true,
    label: " 📊 CRD panels — [P] toggle  · [1–9] drill-in ",
    border: { type: "line" },
    style: { border: { fg: "magenta" }, fg: "white", bg: "default" },
    padding: { left: 1, right: 1 },
  });

  // Drill-in popup (rendered on top of the overlay).
  const detailPopup = blessed.box({
    parent: ctx.screen,
    hidden: true,
    top: "center", left: "center",
    width: "80%", height: "70%",
    tags: true,
    scrollable: true,
    alwaysScroll: true,
    mouse: true,
    keys: true,
    label: " 🔎 CRD detail — [Esc] close ",
    border: { type: "line" },
    style: { border: { fg: "yellow" }, fg: "white", bg: "default" },
    padding: { left: 1, right: 1 },
  });

  let open = false;
  let lastState: ClusterState | null = null;
  let lastRows: CrdRow[] = [];

  function openDetail(idx: number) {
    if (!lastState) return;
    const row = lastRows.find((r) => r.index === idx);
    if (!row) return;
    detailPopup.setContent(renderCrdItemDetail(lastState, row.kind, row.name, row.namespace));
    (detailPopup as any).show();
    (detailPopup as any).setFront();
    (detailPopup as any).focus();
    ctx.screen.render();
  }

  function closeDetail() {
    if (detailPopup.hidden) return;
    (detailPopup as any).hide();
    (overlay as any).focus();
    ctx.screen.render();
  }

  detailPopup.key(["escape", "q"], closeDetail);

  // Numeric drill-in keys (only active while overlay is focused).
  for (const k of ["1", "2", "3", "4", "5", "6", "7", "8", "9"]) {
    overlay.key([k], () => {
      if (!open) return;
      openDetail(parseInt(k, 10));
    });
  }
  overlay.key(["0"], () => { if (open) openDetail(10); });

  return {
    isOpen: () => open,
    hide: () => {
      if (!open) return;
      (overlay as any).hide();
      (detailPopup as any).hide();
      open = false;
      ctx.screen.render();
    },
    toggle: async () => {
      if (ctx.dialogOpen()) return;
      if (open) {
        (overlay as any).hide();
        (detailPopup as any).hide();
        open = false;
        ctx.screen.render();
        return;
      }
      overlay.setContent("{gray-fg}Loading panels…{/}");
      (overlay as any).show();
      (overlay as any).focus();
      open = true;
      ctx.screen.render();
      try {
        const ds = new KubectlDataSource(ctx.kubeContext);
        const state = await ds.fetch();
        lastState = state;
        // Build rows independently so drill-in indices match what the
        // dashboard renders. `renderCrdSections` returns the same rows
        // for default mode; for legacy panel selectors it falls back to
        // the dashboard text without a row index — drill-in is then
        // effectively disabled.
        const { rows } = renderCrdSections(state);
        lastRows = rows;
        overlay.setContent(renderDashboard(state, ctx.panelOpts));
      } catch (e: any) {
        lastState = null;
        lastRows = [];
        overlay.setContent(`{red-fg}panels fetch failed:{/} ${e?.message ?? e}`);
      }
      ctx.screen.render();
    },
  };
}
