// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//
// operator-tui.ts — minimal grid + table widgets backed by plain `blessed`.
//
// These replace the only three `blessed-contrib` widgets the operator
// dashboard used (`grid`, `table`, `log`). `blessed-contrib` pulls in
// vulnerable transitive dependencies (lodash code-injection / prototype
// pollution via GHSA-r5fr-rjxr-66jc + GHSA-f23m-r3pf-42rh, and xml2js
// prototype pollution via `map-canvas` → GHSA-776f-qx25-q3cc) that have no
// consumer-applicable fix — `overrides` in a published package are ignored
// when it is installed as a dependency, so the only durable remedy is to
// stop depending on `blessed-contrib`. `blessed` alone has zero advisories.
//
// The widgets reproduce exactly the surface operator.ts relies on:
//   makeGrid(screen).set(row,col,rowSpan,colSpan, factory, opts) → widget
//   makeTable(opts) → box with .setData({headers,data}), .rows (inner list
//                     exposing .selected/.select/.focus), .show/.hide/.style
// For the activity log, operator.ts uses `blessed.log` directly (its `.log()`
// method matches contrib.log 1:1).

import blessed from "blessed";

type AnyOpts = Record<string, any>;

/** Factory signature shared by blessed.box/list/log and makeTable. */
type WidgetFactory = (opts: AnyOpts) => any;

export interface GridLike {
  set(
    row: number,
    col: number,
    rowSpan: number,
    colSpan: number,
    factory: WidgetFactory,
    opts: AnyOpts,
  ): any;
}

/**
 * Replacement for `new contrib.grid({rows, cols, screen})`. Computes the same
 * percentage-based geometry contrib.grid does and adds a line border by
 * default (contrib.grid borders every cell), then constructs the widget via
 * the supplied factory parented to the screen.
 */
export function makeGrid(screen: any, rows = 12, cols = 12): GridLike {
  const pct = (n: number, d: number): string => `${(n / d) * 100}%`;
  return {
    set(row, col, rowSpan, colSpan, factory, opts = {}) {
      const borderFg = opts?.style?.border?.fg;
      return factory({
        ...opts,
        parent: screen,
        top: pct(row, rows),
        left: pct(col, cols),
        width: pct(colSpan, cols),
        height: pct(rowSpan, rows),
        border: opts.border ?? { type: "line", fg: borderFg },
      });
    },
  };
}

/**
 * Replacement for `contrib.table`. Renders a bordered box containing a fixed
 * header line plus a scrollable `blessed.list` of data rows — the same
 * structure contrib.table uses internally, where the inner list is exposed as
 * `.rows`. Columns are fixed-width (cells are plain strings in the operator,
 * so simple pad/truncate is exact).
 */
export function makeTable(opts: AnyOpts = {}): any {
  const columnWidth: number[] = opts.columnWidth ?? [];
  const columnSpacing: number = opts.columnSpacing ?? 2;
  const sep = " ".repeat(columnSpacing);

  const fmt = (cells: any[]): string =>
    cells
      .map((c, i) => {
        const s = String(c ?? "");
        const w = columnWidth[i];
        if (typeof w !== "number") return s;
        return s.length > w ? s.slice(0, w) : s.padEnd(w);
      })
      .join(sep);

  const box = blessed.box({
    ...opts,
    tags: true,
    border: opts.border ?? { type: "line", fg: opts?.style?.border?.fg },
  });

  const headerLine = blessed.box({
    parent: box,
    top: 0,
    left: 1,
    right: 1,
    height: 1,
    tags: true,
    style: opts?.style?.header ?? { fg: "cyan", bold: true },
  });

  const list = blessed.list({
    parent: box,
    top: 1,
    left: 0,
    right: 0,
    bottom: 0,
    keys: opts.keys ?? false,
    vi: opts.vi ?? false,
    mouse: true,
    tags: true,
    interactive: opts.interactive ?? true,
    style: {
      fg: opts.fg ?? "white",
      selected: opts?.style?.cell?.selected ?? { bg: "blue", fg: "white" },
    },
  });

  (box as any).rows = list;
  (box as any).setData = ({
    headers,
    data,
  }: {
    headers: any[];
    data: any[][];
  }): void => {
    headerLine.setContent(fmt(headers ?? []));
    const prev = (list as any).selected ?? 0;
    list.setItems((data ?? []).map(fmt) as any);
    const max = Math.max(0, (data?.length ?? 0) - 1);
    (list as any).select(Math.min(prev, max));
  };

  return box;
}
