// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Headless blessed surface for the compat suite.
 *
 * The operator TUI (cli/src/commands/operator.ts) does:
 *   import blessed from "blessed";
 *   import contrib from "blessed-contrib";
 *
 *   const screen = blessed.screen({...});
 *   const box = blessed.box({ parent: screen, content: "..." });
 *   screen.key(["q"], () => process.exit(0));
 *   screen.render();
 *
 * The real blessed talks to the tty and maintains a virtual framebuffer.
 * For compat tests we only need:
 *   - a node tree (parent/children, content, keybindings),
 *   - a .render() that records calls,
 *   - a way to inject key events,
 *   - no process.stdout / tty writes.
 *
 * This mock implements exactly that surface. When the operator TUI code
 * imports from here instead of 'blessed', all behaviour is observable.
 *
 * The mock is deliberately dumb — it is not a blessed re-implementation.
 * It preserves the parent/child shape and exposes a .snapshot() for oracles.
 */

export type NodeKind =
  | "screen"
  | "box"
  | "text"
  | "list"
  | "table"
  | "log"
  | "line"
  | "bar"
  | "sparkline"
  | "grid"
  | "unknown";

export interface MockNodeOptions {
  kind?: NodeKind;
  parent?: MockNode;
  content?: string;
  label?: string;
  [key: string]: unknown;
}

export class MockNode {
  readonly kind: NodeKind;
  readonly options: MockNodeOptions;
  parent: MockNode | null = null;
  readonly children: MockNode[] = [];
  private _content: string;
  private readonly keyBindings = new Map<string, Array<(ch: string, key: unknown) => void>>();
  private destroyed = false;

  constructor(options: MockNodeOptions = {}) {
    this.kind = options.kind ?? "unknown";
    this.options = options;
    this._content = typeof options.content === "string" ? options.content : "";
    if (options.parent) {
      options.parent.append(this);
    }
  }

  append(child: MockNode): void {
    if (child.parent) {
      const idx = child.parent.children.indexOf(child);
      if (idx >= 0) child.parent.children.splice(idx, 1);
    }
    child.parent = this;
    this.children.push(child);
  }

  remove(child: MockNode): void {
    const idx = this.children.indexOf(child);
    if (idx >= 0) {
      this.children.splice(idx, 1);
      child.parent = null;
    }
  }

  destroy(): void {
    this.destroyed = true;
    if (this.parent) this.parent.remove(this);
  }

  isDestroyed(): boolean {
    return this.destroyed;
  }

  setContent(s: string): void {
    this._content = s;
  }

  getContent(): string {
    return this._content;
  }

  setLabel(l: string): void {
    this.options.label = l;
  }

  setData(_key: string, _val: unknown): void { /* no-op for compat */ }
  hide(): void { /* no-op for compat */ }
  show(): void { /* no-op for compat */ }
  focus(): void { /* no-op for compat; real focus tracked by Screen */ }

  // blessed keybinding surface — accepts a key or array of keys
  key(keys: string | string[], handler: (ch: string, key: unknown) => void): void {
    const arr = Array.isArray(keys) ? keys : [keys];
    for (const k of arr) {
      const list = this.keyBindings.get(k);
      if (list) list.push(handler);
      else this.keyBindings.set(k, [handler]);
    }
  }

  // harness-only: drive a key event as if the user typed it
  emitKey(k: string, ch = ""): boolean {
    const handlers = this.keyBindings.get(k);
    if (!handlers || handlers.length === 0) return false;
    for (const h of handlers) h(ch, { name: k });
    return true;
  }

  hasKeyBinding(k: string): boolean {
    return this.keyBindings.has(k);
  }

  // dummy no-op event emitter surface
  on(_event: string, _handler: (...args: unknown[]) => void): this { return this; }
  off(_event: string, _handler: (...args: unknown[]) => void): this { return this; }
  emit(_event: string, ..._args: unknown[]): boolean { return false; }
}

export class MockScreen extends MockNode {
  renderCount = 0;
  title = "";

  constructor(options: MockNodeOptions = {}) {
    super({ ...options, kind: "screen" });
  }

  render(): void {
    this.renderCount += 1;
  }

  // snapshot the whole tree as a plain JSON-friendly object
  snapshot(): Record<string, unknown> {
    const walk = (n: MockNode): Record<string, unknown> => ({
      kind: n.kind,
      label: n.options.label ?? null,
      content: n.getContent(),
      keys: Array.from((n as unknown as { keyBindings: Map<string, unknown> }).keyBindings?.keys?.() ?? []),
      children: n.children.map(walk),
    });
    return walk(this);
  }

  // harness: walk and collect all nodes of a kind
  findByKind(kind: NodeKind): MockNode[] {
    const out: MockNode[] = [];
    const walk = (n: MockNode) => {
      if (n.kind === kind) out.push(n);
      for (const c of n.children) walk(c);
    };
    walk(this);
    return out;
  }

  // dispatch a key event; screen forwards to every node with a binding
  typeKey(k: string, ch = ""): { deliveredTo: number } {
    let delivered = 0;
    const walk = (n: MockNode) => {
      if (n.emitKey(k, ch)) delivered += 1;
      for (const c of n.children) walk(c);
    };
    walk(this);
    return { deliveredTo: delivered };
  }
}

// Factory mirroring `blessed.screen()` / `blessed.box()` ...
// blessed's real signature accepts a single options object.
export interface BlessedMockApi {
  screen: (opts?: MockNodeOptions) => MockScreen;
  box: (opts?: MockNodeOptions) => MockNode;
  text: (opts?: MockNodeOptions) => MockNode;
  list: (opts?: MockNodeOptions) => MockNode;
  log: (opts?: MockNodeOptions) => MockNode;
  line: (opts?: MockNodeOptions) => MockNode;
  bar: (opts?: MockNodeOptions) => MockNode;
  sparkline: (opts?: MockNodeOptions) => MockNode;
}

export const blessedMock: BlessedMockApi = {
  screen: (opts) => new MockScreen(opts),
  box: (opts) => new MockNode({ ...opts, kind: "box" }),
  text: (opts) => new MockNode({ ...opts, kind: "text" }),
  list: (opts) => new MockNode({ ...opts, kind: "list" }),
  log: (opts) => new MockNode({ ...opts, kind: "log" }),
  line: (opts) => new MockNode({ ...opts, kind: "line" }),
  bar: (opts) => new MockNode({ ...opts, kind: "bar" }),
  sparkline: (opts) => new MockNode({ ...opts, kind: "sparkline" }),
};

// blessed-contrib surfaces used by operator.ts (line, table, log, bar, sparkline, gauge).
// A contrib "grid" is just a factory for positioned nodes; we return the bare MockNode.
export const blessedContribMock = {
  grid: class {
    rows: number; cols: number; screen: MockScreen;
    constructor(opts: { rows: number; cols: number; screen: MockScreen }) {
      this.rows = opts.rows; this.cols = opts.cols; this.screen = opts.screen;
    }
    set(_r: number, _c: number, _rs: number, _cs: number, factory: (opts: MockNodeOptions) => MockNode, opts: MockNodeOptions = {}): MockNode {
      const node = factory({ ...opts, parent: this.screen });
      return node;
    }
  },
  line: (opts: MockNodeOptions = {}) => new MockNode({ ...opts, kind: "line" }),
  table: (opts: MockNodeOptions = {}) => new MockNode({ ...opts, kind: "table" }),
  log: (opts: MockNodeOptions = {}) => new MockNode({ ...opts, kind: "log" }),
  bar: (opts: MockNodeOptions = {}) => new MockNode({ ...opts, kind: "bar" }),
  sparkline: (opts: MockNodeOptions = {}) => new MockNode({ ...opts, kind: "sparkline" }),
  gauge: (opts: MockNodeOptions = {}) => new MockNode({ ...opts, kind: "box" }),
};
