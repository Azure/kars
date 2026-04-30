/**
 * Operator-TUI panels overlay (S14) — wires the modular-panels view as
 * a blessed overlay box on the main dashboard. Extracted out of
 * `operator.ts` so the dashboard module stays under §4.2's 800-LOC cap.
 */
import blessed from "blessed";
import { KubectlDataSource, renderDashboard } from "./panels/index.js";

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
    label: " 📊 Panels (S14) — [P] toggle ",
    border: { type: "line" },
    style: { border: { fg: "magenta" }, fg: "white", bg: "default" },
    padding: { left: 1, right: 1 },
  });

  let open = false;

  return {
    isOpen: () => open,
    hide: () => {
      if (!open) return;
      (overlay as any).hide();
      open = false;
      ctx.screen.render();
    },
    toggle: async () => {
      if (ctx.dialogOpen()) return;
      if (open) {
        (overlay as any).hide();
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
        overlay.setContent(renderDashboard(state, ctx.panelOpts));
      } catch (e: any) {
        overlay.setContent(`{red-fg}panels fetch failed:{/} ${e?.message ?? e}`);
      }
      ctx.screen.render();
    },
  };
}
