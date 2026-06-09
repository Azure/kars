// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars showcase — pitch deck v2 (practitioner-grade)
//
// Visual language:
//   • Pillar overview slides     → Patrick Collison / Stripe Press style:
//       heading + 2-3 line paragraph in real prose + a row of named primitives
//   • Architecture / mesh / sandbox → Bret Victor style:
//       one named artefact per slide with real labels (real CRD field names,
//       real iptables rules, real protocol fields) — not just abstract labels
//   • Code / governance slides   → Stripe-docs style:
//       monospace code/config block, side-by-side with prose explanation
//
// Typography: Helvetica display + Helvetica body, Consolas for code.
// Dark sandwich (dark intro/close), light content.
// Single accent (teal #028090). Generous whitespace. Real content density.

const pptxgen = require("pptxgenjs");

const pres = new pptxgen();
pres.layout = "LAYOUT_WIDE"; // 13.3 × 7.5
pres.author = "kars";
pres.title = "kars — secure AI agent runtime on Kubernetes";

// ── palette ──────────────────────────────────────────────
const INK = "1A1A1A";       // body text
const PAPER = "FFFFFF";
const NIGHT = "0A0E1A";     // intro/close
const MUTED = "6E7681";     // secondary
const QUIET = "AFB3BD";     // tertiary (footers, captions)
const ACCENT = "028090";    // teal — single accent
const ACCENT_LIGHT = "E6F1F2";
const CODE_BG = "F6F8FA";
const CODE_KW = "C53030";   // keywords in code
const CODE_STR = "276749";  // strings in code

const F_DISPLAY = "Helvetica";
const F_BODY = "Helvetica";
const F_CODE = "Consolas";

const W = 13.3;
const H = 7.5;
const M = 0.7;              // outer margin

// ── slide types ──────────────────────────────────────────
function dark() {
  const s = pres.addSlide();
  s.background = { color: NIGHT };
  return s;
}
function light() {
  const s = pres.addSlide();
  s.background = { color: PAPER };
  return s;
}

// eyebrow label (Stripe-press style, all caps, kerned)
function eyebrow(s, txt, color = MUTED) {
  s.addText(txt.toUpperCase(), {
    x: M, y: 0.55, w: W - 2 * M, h: 0.3,
    fontFace: F_BODY, fontSize: 11, charSpacing: 4,
    color, margin: 0,
  });
}

// page-number footer (gives a sense of book-ness)
function pageNum(s, n) {
  s.addText(String(n).padStart(2, "0"), {
    x: W - M - 0.4, y: H - 0.5, w: 0.4, h: 0.3,
    fontFace: F_BODY, fontSize: 10, color: QUIET,
    align: "right", margin: 0,
  });
}

// large slide title (40-52pt, left-aligned, no underline accent)
function title(s, txt, opts = {}) {
  s.addText(txt, {
    x: M, y: opts.y ?? 1.0, w: W - 2 * M, h: opts.h ?? 1.4,
    fontFace: F_DISPLAY, fontSize: opts.fontSize ?? 46, bold: true,
    color: opts.color ?? INK, align: "left", valign: "top", margin: 0,
  });
}

// lede paragraph (16-20pt, sets context; this is what makes it practitioner-grade)
function lede(s, txt, opts = {}) {
  s.addText(txt, {
    x: M, y: opts.y ?? 2.6, w: opts.w ?? (W - 2 * M), h: opts.h ?? 1.6,
    fontFace: F_BODY, fontSize: opts.fontSize ?? 18,
    color: opts.color ?? INK, align: "left", valign: "top", margin: 0,
    paraSpaceAfter: 8,
  });
}

// row of named primitives (small monospace label + short prose)
function primitiveRow(s, items, opts = {}) {
  const y0 = opts.y ?? 5.0;
  const totalW = W - 2 * M;
  const w = (totalW - (items.length - 1) * 0.4) / items.length;
  items.forEach(([code, prose], i) => {
    const x = M + i * (w + 0.4);
    // thin teal rule above each
    s.addShape(pres.shapes.LINE, {
      x, y: y0, w: 1.2, h: 0,
      line: { color: ACCENT, width: 1.5 },
    });
    s.addText(code, {
      x, y: y0 + 0.1, w, h: 0.4,
      fontFace: F_CODE, fontSize: 13, color: ACCENT, margin: 0,
    });
    s.addText(prose, {
      x, y: y0 + 0.55, w, h: 1.4,
      fontFace: F_BODY, fontSize: 13, color: INK, margin: 0,
      paraSpaceAfter: 4,
    });
  });
}

// code block — monospace with simple tokenization (keywords + strings)
// Accepts either a plain string or an array of {text, kind} runs.
function codeBlock(s, runs, opts = {}) {
  const x = opts.x ?? M;
  const y = opts.y ?? 2.6;
  const w = opts.w ?? 7.0;
  const h = opts.h ?? 4.0;
  s.addShape(pres.shapes.RECTANGLE, {
    x, y, w, h,
    fill: { color: CODE_BG }, line: { color: "E1E4E8", width: 0.75 },
  });
  if (typeof runs === "string") {
    s.addText(runs, {
      x: x + 0.25, y: y + 0.25, w: w - 0.5, h: h - 0.5,
      fontFace: F_CODE, fontSize: opts.fontSize ?? 13, color: INK,
      align: "left", valign: "top", margin: 0,
    });
  } else {
    s.addText(runs.map(r => {
      const o = { breakLine: r.br === true };
      if (r.k === "kw") o.color = CODE_KW;
      else if (r.k === "str") o.color = CODE_STR;
      else if (r.k === "muted") o.color = MUTED;
      else o.color = INK;
      o.bold = r.b === true;
      return { text: r.t, options: o };
    }), {
      x: x + 0.25, y: y + 0.25, w: w - 0.5, h: h - 0.5,
      fontFace: F_CODE, fontSize: opts.fontSize ?? 12,
      align: "left", valign: "top", margin: 0,
    });
  }
}

// right-column prose paired with codeBlock
function rightProse(s, paragraphs, opts = {}) {
  const x = opts.x ?? 8.1;
  const y = opts.y ?? 2.6;
  const w = opts.w ?? (W - x - M);
  s.addText(
    paragraphs.map((p, i) => ({
      text: p,
      options: { breakLine: i < paragraphs.length - 1, paraSpaceAfter: 8 },
    })),
    {
      x, y, w, h: 4.0,
      fontFace: F_BODY, fontSize: 14, color: INK,
      align: "left", valign: "top", margin: 0,
    }
  );
}

// section divider (very minimal — used between major narrative arcs)
function section(s, n, txt) {
  s.addText(`§ ${n}`, {
    x: M, y: 2.5, w: W - 2 * M, h: 0.4,
    fontFace: F_BODY, fontSize: 13, color: QUIET, charSpacing: 4, margin: 0,
  });
  s.addText(txt, {
    x: M, y: 3.0, w: W - 2 * M, h: 1.6,
    fontFace: F_DISPLAY, fontSize: 56, bold: true, color: INK, margin: 0,
  });
}


// ════════════════════════════════════════════════════════════════════════════
// SLIDES — 21-slide structure, outcome-first, architecture-deep, kubernetes-native
// ════════════════════════════════════════════════════════════════════════════

let page = 1;

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 1: TITLE (dark, magazine-style)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = dark();
  s.addText("kars", {
    x: M, y: 2.4, w: W - 2 * M, h: 2.2,
    fontFace: F_DISPLAY, fontSize: 168, bold: true, color: PAPER,
    align: "left", margin: 0,
  });
  s.addText("Secure AI agent runtime on Kubernetes.", {
    x: M, y: 4.8, w: W - 2 * M, h: 0.5,
    fontFace: F_BODY, fontSize: 22, color: ACCENT_LIGHT, align: "left", margin: 0,
  });
  s.addText("Built on the Microsoft Agent Governance Toolkit.", {
    x: M, y: 5.4, w: W - 2 * M, h: 0.5,
    fontFace: F_BODY, fontSize: 16, color: QUIET, align: "left", margin: 0,
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 2: THE OUTCOME (concrete, measurable, non-competitive)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§1 · the outcome");
  title(s, "What you actually get.");
  lede(s,
    "kars turns agent execution into Kubernetes-managed infrastructure:  declared, " +
    "reconciled, isolated, governed, and observable. Every inference call, every tool " +
    "invocation, every egress byte flows through one policy plane and lands as a record " +
    "you can audit, rate-limit, time-box, and revoke.",
    { y: 2.7, h: 1.9 }
  );
  primitiveRow(s, [
    ["every call audited",
      "policy decision + hash-chained JSONL row, per call, per sandbox"],
    ["sandbox in minutes",
      "kubectl apply KarsSandbox → Running 2/2 in ≈60 s on AKS"],
    ["one policy plane",
      "models · MCP · A2A · tools · egress · memory — one CRD set"],
    ["default-deny egress",
      "time-boxed approvals carry reason + ticket; auto-expire to None"],
  ], { y: 5.0 });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 3: WHAT KARS IS + FOUR PILLARS (merged, dark statement + bottom row)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = dark();
  page++;
  eyebrow(s, "§2 · what kars is", QUIET);
  s.addText("Secure, multi-runtime AI agent runtime on Azure Kubernetes Service.",
    {
      x: M, y: 1.5, w: W - 2 * M, h: 1.8,
      fontFace: F_DISPLAY, fontSize: 36, bold: true, color: PAPER,
      align: "left", valign: "top", margin: 0,
    });
  s.addText("End-to-end encrypted inter-agent mesh.  Governance enforced inside the per-sandbox data plane.",
    {
      x: M, y: 3.3, w: W - 2 * M, h: 1.0,
      fontFace: F_DISPLAY, fontSize: 24, bold: false, color: ACCENT_LIGHT,
      align: "left", valign: "top", margin: 0,
    });
  // four pillars row at bottom (compressed, on dark)
  const pillars = [
    ["sandbox/", "kars-strict seccomp · iptables egress-guard · drop ALL caps"],
    ["agentmesh/", "Signal Protocol · X3DH · Double Ratchet · KNOCK"],
    ["router/", "InferencePolicy · ToolPolicy · Content Safety · budgets"],
    ["contract/v1", "KARS_MODEL · KARS_RUNTIME_KIND · 127.0.0.1:8443"],
  ];
  const totalW = W - 2 * M;
  const pw = (totalW - 3 * 0.4) / 4;
  const py = 5.2;
  pillars.forEach(([code, prose], i) => {
    const x = M + i * (pw + 0.4);
    s.addShape(pres.shapes.LINE, {
      x, y: py, w: 1.2, h: 0,
      line: { color: ACCENT, width: 1.5 },
    });
    s.addText(code, {
      x, y: py + 0.1, w: pw, h: 0.4,
      fontFace: F_CODE, fontSize: 13, color: ACCENT_LIGHT, margin: 0,
    });
    s.addText(prose, {
      x, y: py + 0.55, w: pw, h: 1.4,
      fontFace: F_BODY, fontSize: 12, color: ACCENT_LIGHT, margin: 0,
      paraSpaceAfter: 4,
    });
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 4: HIGH-LEVEL ARCHITECTURE — one diagram, the whole shape
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3 · architecture · the shape");
  title(s, "How it fits together.", { fontSize: 40 });
  lede(s,
    "Two Rust binaries.  Eleven CRDs.  One pod shape.  The Kubernetes API server is the " +
    "source of truth — the rest is reconciliation.",
    { y: 2.5, h: 0.7 }
  );

  // ── kubectl apply line, top
  const kx = M, ky = 3.3;
  s.addText("kubectl apply  ▸", {
    x: kx, y: ky, w: 2.2, h: 0.3,
    fontFace: F_CODE, fontSize: 11, color: MUTED, margin: 0,
  });

  // ── Cluster boundary (rounded, dashed) ──
  const cx = M, cy = 3.6, cw = W - 2 * M, ch = 2.6;
  s.addShape(pres.shapes.ROUNDED_RECTANGLE, {
    x: cx, y: cy, w: cw, h: ch,
    rectRadius: 0.15,
    fill: { color: PAPER }, line: { color: ACCENT, width: 1.5, dashType: "dash" },
  });
  s.addText("AKS / kind cluster", {
    x: cx + 0.3, y: cy + 0.08, w: 3, h: 0.3,
    fontFace: F_CODE, fontSize: 10, color: ACCENT, margin: 0,
  });

  // Three internal boxes
  const bw = (cw - 0.6 - 2 * 0.3) / 3;
  const by = cy + 0.5;
  const bh = ch - 0.7;
  const bx0 = cx + 0.3;

  // ── Box 1: CRDs (left)
  s.addShape(pres.shapes.RECTANGLE, {
    x: bx0, y: by, w: bw, h: bh,
    fill: { color: "F6F8FA" }, line: { color: ACCENT, width: 0.75 },
  });
  s.addText("kars CRDs", {
    x: bx0 + 0.15, y: by + 0.1, w: bw - 0.3, h: 0.3,
    fontFace: F_DISPLAY, fontSize: 13, bold: true, color: INK, margin: 0,
  });
  s.addText("(11 kinds · the API contract)", {
    x: bx0 + 0.15, y: by + 0.42, w: bw - 0.3, h: 0.25,
    fontFace: F_BODY, fontSize: 9.5, color: MUTED, margin: 0,
  });
  s.addText("KarsSandbox\nInferencePolicy\nToolPolicy · EgressApproval\nKarsMemory · TrustGraph\nA2AAgent · McpServer · …", {
    x: bx0 + 0.15, y: by + 0.75, w: bw - 0.3, h: bh - 0.85,
    fontFace: F_CODE, fontSize: 10, color: INK, margin: 0,
    paraSpaceAfter: 2,
  });

  // ── Box 2: Controller (middle)
  const bx1 = bx0 + bw + 0.3;
  s.addShape(pres.shapes.RECTANGLE, {
    x: bx1, y: by, w: bw, h: bh,
    fill: { color: "F6F8FA" }, line: { color: ACCENT, width: 0.75 },
  });
  s.addText("kars-controller", {
    x: bx1 + 0.15, y: by + 0.1, w: bw - 0.3, h: 0.3,
    fontFace: F_DISPLAY, fontSize: 13, bold: true, color: INK, margin: 0,
  });
  s.addText("(Rust · kube-rs operator)", {
    x: bx1 + 0.15, y: by + 0.42, w: bw - 0.3, h: 0.25,
    fontFace: F_BODY, fontSize: 9.5, color: MUTED, margin: 0,
  });
  s.addText(
    [
      { text: "watches all 11 CRDs", options: { breakLine: true, paraSpaceAfter: 4 } },
      { text: "compiles policy → ConfigMaps", options: { breakLine: true, paraSpaceAfter: 4 } },
      { text: "reconciles desired state", options: { breakLine: true, paraSpaceAfter: 4 } },
      { text: "stamps .status.phase + conditions", options: {} },
    ],
    {
      x: bx1 + 0.15, y: by + 0.75, w: bw - 0.3, h: bh - 0.85,
      fontFace: F_BODY, fontSize: 10.5, color: INK, margin: 0,
    });

  // ── Box 3: Sandbox pod (right)
  const bx2 = bx1 + bw + 0.3;
  s.addShape(pres.shapes.RECTANGLE, {
    x: bx2, y: by, w: bw, h: bh,
    fill: { color: "F6F8FA" }, line: { color: ACCENT, width: 0.75 },
  });
  s.addText("Sandbox pod", {
    x: bx2 + 0.15, y: by + 0.1, w: bw - 0.3, h: 0.3,
    fontFace: F_DISPLAY, fontSize: 13, bold: true, color: INK, margin: 0,
  });
  s.addText("(per KarsSandbox CR)", {
    x: bx2 + 0.15, y: by + 0.42, w: bw - 0.3, h: 0.25,
    fontFace: F_BODY, fontSize: 9.5, color: MUTED, margin: 0,
  });
  // Two container chips inside
  const ccy = by + 0.78, cch = 0.42;
  s.addShape(pres.shapes.RECTANGLE, {
    x: bx2 + 0.15, y: ccy, w: bw - 0.3, h: cch,
    fill: { color: PAPER }, line: { color: MUTED, width: 0.5 },
  });
  s.addText("agent  (UID 1000)", {
    x: bx2 + 0.25, y: ccy, w: bw - 0.5, h: cch,
    fontFace: F_CODE, fontSize: 10, color: INK, valign: "middle", margin: 0,
  });
  s.addShape(pres.shapes.RECTANGLE, {
    x: bx2 + 0.15, y: ccy + cch + 0.06, w: bw - 0.3, h: cch,
    fill: { color: PAPER }, line: { color: ACCENT, width: 0.75 },
  });
  s.addText("inference-router  (1001)", {
    x: bx2 + 0.25, y: ccy + cch + 0.06, w: bw - 0.5, h: cch,
    fontFace: F_CODE, fontSize: 10, color: ACCENT, valign: "middle", margin: 0,
  });

  // Egress label outside, bottom-right corner of cluster
  s.addText("◀  only path out  ▶", {
    x: W - M - 2.1, y: cy + ch + 0.05, w: 2.1, h: 0.3,
    fontFace: F_CODE, fontSize: 10, color: ACCENT, align: "right", margin: 0,
  });

  // Bottom row: external services
  const ey = cy + ch + 0.4;
  const services = ["Azure OpenAI", "Anthropic", "OpenAI", "Bedrock", "MCP", "A2A peers", "AGT relay"];
  const sgap = 0.12;
  const stotal = W - 2 * M;
  const sw = (stotal - (services.length - 1) * sgap) / services.length;
  services.forEach((sv, i) => {
    const x = M + i * (sw + sgap);
    s.addShape(pres.shapes.ROUNDED_RECTANGLE, {
      x, y: ey, w: sw, h: 0.4,
      rectRadius: 0.05,
      fill: { color: ACCENT_LIGHT }, line: { color: ACCENT, width: 0.5 },
    });
    s.addText(sv, {
      x, y: ey, w: sw, h: 0.4,
      fontFace: F_BODY, fontSize: 10, color: INK,
      align: "center", valign: "middle", margin: 0,
    });
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 5: THE CORE — Controller / CRDs / Inference Router
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3.1 · architecture · the core");
  title(s, "Three components.  One contract.", { fontSize: 40 });
  lede(s,
    "Two Rust binaries and a set of CRDs do the work.  The controller turns desired " +
    "state into Kubernetes primitives.  The router sits inside every sandbox pod and " +
    "is the single egress path. The CRDs are the contract between them.",
    { y: 2.55, h: 1.4 }
  );

  // 3 columns — boxes for each component
  const boxes = [
    {
      title: "Controller",
      mono: "controller/src/main.rs",
      lines: [
        "Rust 1.88 · kube-rs operator",
        "11 reconcilers (one per CRD kind)",
        "leader election · backoff · drift watch",
        "compiles policy → ConfigMaps",
        "stamps .status.phase + conditions",
      ],
    },
    {
      title: "CRDs",
      mono: "deploy/helm/kars/templates/crd-*.yaml",
      lines: [
        "11 kinds (9 namespaced · 2 cluster)",
        "OpenAPIv3 + Admission Policies",
        "owned by controller · GC via ownerRefs",
        "single source of truth",
        "kubectl-native diff/apply/explain",
      ],
    },
    {
      title: "Inference Router",
      mono: "inference-router/src/main.rs",
      lines: [
        "Rust 1.88 · axum sidecar (UID 1001)",
        ":8443 admin  ·  :8444 transparent proxy",
        "single egress path for the agent",
        "policy gate · audit · budget · failover",
        "IMDS / Workload Identity — no keys in pod",
      ],
    },
  ];
  const bw = (W - 2 * M - 2 * 0.35) / 3;
  const by = 4.4;
  const bh = 2.6;
  boxes.forEach((b, i) => {
    const x = M + i * (bw + 0.35);
    s.addShape(pres.shapes.RECTANGLE, {
      x, y: by, w: bw, h: bh,
      fill: { color: PAPER }, line: { color: ACCENT, width: 1 },
    });
    s.addText(b.title, {
      x: x + 0.2, y: by + 0.15, w: bw - 0.4, h: 0.45,
      fontFace: F_DISPLAY, fontSize: 18, bold: true, color: INK, margin: 0,
    });
    s.addText(b.mono, {
      x: x + 0.2, y: by + 0.55, w: bw - 0.4, h: 0.3,
      fontFace: F_CODE, fontSize: 10, color: ACCENT, margin: 0,
    });
    s.addText(
      b.lines.map((l, j) => ({
        text: "·  " + l,
        options: { breakLine: j < b.lines.length - 1, paraSpaceAfter: 4 },
      })),
      {
        x: x + 0.2, y: by + 0.95, w: bw - 0.4, h: bh - 1.1,
        fontFace: F_BODY, fontSize: 11.5, color: INK, margin: 0,
      });
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 5: ROUTER REQUEST FLOW — every agent call, six named stages
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3.2 · architecture · request path");
  title(s, "Every call:  one hop, one decision, one record.", { fontSize: 34 });
  lede(s,
    "The agent process never sees an API key, never sees an upstream URL, never knows " +
    "the difference between Foundry, Anthropic, an MCP server, or an A2A peer.  iptables " +
    "rewrites every outbound to the router sidecar.  The rest is policy and bookkeeping.",
    { y: 2.5, h: 1.5 }
  );

  // Six stages — vertical numbered list, each row: [N] | <stage> | <detail>
  const stages = [
    ["1", "agent process",       "UID 1000 makes a plain HTTPS call — no SDK, no special client"],
    ["2", "iptables DNAT",       "kars-strict redirects :80/:443 → 127.0.0.1:8444  (egress-guard init-container)"],
    ["3", "router :8444",        "transparent proxy terminates TLS, reconstitutes the request, looks up policy"],
    ["4", "policy gate",         "InferencePolicy · ToolPolicy · EgressApproval · Content Safety prompt-shields"],
    ["5", "budget + audit",      "per-sandbox token meter ticks · hash-chained JSONL row written · OTel emitted"],
    ["6", "upstream",            "Azure OpenAI · Anthropic · OpenAI · Bedrock · MCP · A2A — chosen by failover chain"],
  ];
  const sy = 4.2;
  const sh = 0.46;
  stages.forEach(([n, stage, detail], i) => {
    const y = sy + i * sh;
    s.addText(n, {
      x: M, y, w: 0.45, h: sh,
      fontFace: F_CODE, fontSize: 16, bold: true, color: ACCENT, margin: 0, align: "left",
    });
    s.addText(stage, {
      x: M + 0.55, y, w: 2.4, h: sh,
      fontFace: F_DISPLAY, fontSize: 14, bold: true, color: INK, margin: 0,
    });
    s.addText(detail, {
      x: M + 3.05, y, w: W - M - 3.05 - M, h: sh,
      fontFace: F_BODY, fontSize: 12, color: INK, margin: 0,
    });
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 6: INFERENCE ROUTER INTERNALS — routes + subsystems (2-column)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3.3 · architecture · router internals");
  title(s, "Inside the sidecar.", { fontSize: 38 });
  lede(s,
    "axum routes are mounted as merged sub-routers (inference, foundry, mesh, mcp, a2a, " +
    "handoff, spawn, governance, egress, admin).  Cross-cutting subsystems are wired once " +
    "as Tower layers and reused by every route.",
    { y: 2.5, h: 1.2 }
  );

  // LEFT COLUMN — routes
  const leftX = M, leftW = 6.0;
  s.addText("Routes", {
    x: leftX, y: 3.95, w: leftW, h: 0.4,
    fontFace: F_DISPLAY, fontSize: 16, bold: true, color: INK, margin: 0,
  });
  const routes = [
    ["/v1/chat/completions",  "OpenAI-compatible inference (translated to Azure / Anthropic)"],
    ["/v1/messages",           "Anthropic Messages API · native passthrough"],
    ["/mcp",                   "Model Context Protocol · Streamable HTTP + OAuth"],
    ["/a2a + /.well-known/",   "A2A 1.0.0 JSON-RPC + signed Agent Card (Ed25519)"],
    ["/mesh",                  "WebSocket proxy to AGT relay (Signal-encrypted bytes only)"],
    ["/spawn",                 "sub-agent spawn — governance-gated, parent-AMID-attested"],
    ["/handoff/*",             "agent succession with token replay + drain semantics"],
    ["/admin/* + /metrics",    "operator API · Prometheus exporters · health probes"],
  ];
  const ry0 = 4.4;
  routes.forEach(([path, prose], i) => {
    const y = ry0 + i * 0.36;
    s.addText(path, {
      x: leftX, y, w: 2.55, h: 0.32,
      fontFace: F_CODE, fontSize: 10.5, color: ACCENT, margin: 0,
    });
    s.addText(prose, {
      x: leftX + 2.6, y, w: leftW - 2.6, h: 0.32,
      fontFace: F_BODY, fontSize: 10.5, color: INK, margin: 0,
    });
  });

  // RIGHT COLUMN — subsystems
  const rightX = 7.0, rightW = W - 7.0 - M;
  s.addText("Subsystems", {
    x: rightX, y: 3.95, w: rightW, h: 0.4,
    fontFace: F_DISPLAY, fontSize: 16, bold: true, color: INK, margin: 0,
  });
  const subs = [
    ["audit",          "hash-chained JSONL — prev_hash / hash per row"],
    ["budget",         "per-sandbox token meter · per-tool RPS · 429 on exhaustion"],
    ["failover",       "provider chain · 503/RateLimit auto-fallback · Copilot JWT refresh"],
    ["content safety", "Azure prompt-shields · jailbreak detection · blocked-output rewrite"],
    ["trust store",    "Ed25519 identities · X25519 keys · X3DH bundles · Double Ratchet sessions"],
    ["auth",           "IMDS / Workload Identity · no API keys ever land in the pod"],
    ["mtls",           "A2A inter-cluster mTLS · trust-graph projection"],
    ["mesh transport", "AGT MeshClient · KNOCK proof-of-possession · sealed-sender mode"],
  ];
  subs.forEach(([code, prose], i) => {
    const y = ry0 + i * 0.36;
    s.addText(code, {
      x: rightX, y, w: 1.8, h: 0.32,
      fontFace: F_CODE, fontSize: 10.5, color: ACCENT, margin: 0,
    });
    s.addText(prose, {
      x: rightX + 1.8, y, w: rightW - 1.8, h: 0.32,
      fontFace: F_BODY, fontSize: 10.5, color: INK, margin: 0,
    });
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 7: CONTROLLER RECONCILIATION — KarsSandbox → 9 k8s primitives
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3.4 · architecture · the controller loop");
  title(s, "KarsSandbox  →  nine Kubernetes primitives.", { fontSize: 30 });
  lede(s,
    "Reconciliation is idempotent.  Every apply walks nine steps under server-side apply " +
    "with field managers per step (so other controllers can co-own pieces).  ownerRefs " +
    "guarantee garbage collection — delete the KarsSandbox, the namespace and everything " +
    "in it go too.  Source:  controller/src/reconciler/mod.rs:668-2927.",
    { y: 2.45, h: 1.6 }
  );

  // 9 primitives in two columns (5 left, 4 right)
  const items = [
    ["1", "Namespace",            "kars-<name> — isolation boundary; only the controller-managed objects exist here"],
    ["2", "ServiceAccount + WI",  "Azure Workload Identity binding · per-sandbox FedCred · short-lived OIDC tokens"],
    ["3", "NetworkPolicy",        "default-deny + allowlist · ingress :8443 from monitoring · DNS + IMDS only egress"],
    ["4", "ConfigMaps",           "policy bundles (Inference + Tool + Egress + Memory) · AGT envelope · run-tag"],
    ["5", "Secret",               "gateway token (rotated) · router admin token · seccomp profile path"],
    ["6", "Deployment",           "1 init container (egress-guard) + 2 app containers (openclaw + inference-router)"],
    ["7", "Service",              "cluster-internal :8443 router admin + :18789 OpenClaw WebUI"],
    ["8", "ClusterRoleBinding",   "spawn permissions — sandbox SA may create sub-sandbox KarsSandbox CRs"],
    ["9", "CronJob",              "blocklist refresh every 6 h — pulls signed allowlist deltas + updates router CM"],
  ];
  const iy0 = 4.25;
  const ih = 0.32;
  items.forEach(([n, name, prose], i) => {
    const y = iy0 + i * ih;
    s.addText(n, {
      x: M, y, w: 0.35, h: ih,
      fontFace: F_CODE, fontSize: 12, bold: true, color: ACCENT, margin: 0,
    });
    s.addText(name, {
      x: M + 0.4, y, w: 2.0, h: ih,
      fontFace: F_DISPLAY, fontSize: 12, bold: true, color: INK, margin: 0,
    });
    s.addText(prose, {
      x: M + 2.45, y, w: W - M - 2.45 - M, h: ih,
      fontFace: F_BODY, fontSize: 10.5, color: INK, margin: 0,
    });
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 8: STATUS & OBSERVABILITY — the operator contract
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3.5 · architecture · status");
  title(s, "Status is the operator contract.", { fontSize: 32 });
  lede(s,
    "Every CRD carries `.status.phase` from a fixed taxonomy, generation-tracked conditions, " +
    "last-reconciled timestamp, and a degradation reason. Drift surfaces as Kubernetes " +
    "Warning Events.  The router emits Prometheus counters + GenAI OpenTelemetry.",
    { y: 2.45, h: 1.7 }
  );

  // LEFT: real status snippet (code block)
  codeBlock(s, [
    { t: "status:\n" },
    { t: "  phase: ", k: "kw" }, { t: "Running\n", k: "str" },
    { t: "  observedGeneration: ", k: "kw" }, { t: "7\n", k: "str" },
    { t: "  lastReconciled: ", k: "kw" }, { t: "\"2026-06-08T18:23:11Z\"\n", k: "str" },
    { t: "  conditions:\n" },
    { t: "    - type: ", k: "kw" }, { t: "PolicyCompiled\n", k: "str" },
    { t: "      status: ", k: "kw" }, { t: "\"True\"\n", k: "str" },
    { t: "      reason: ", k: "kw" }, { t: "EnvelopeApplied\n", k: "str" },
    { t: "    - type: ", k: "kw" }, { t: "RouterEnforcing\n", k: "str" },
    { t: "      status: ", k: "kw" }, { t: "\"True\"\n", k: "str" },
    { t: "      reason: ", k: "kw" }, { t: "ConfirmedDigest\n", k: "str" },
    { t: "    - type: ", k: "kw" }, { t: "Available\n", k: "str" },
    { t: "      status: ", k: "kw" }, { t: "\"True\"\n", k: "str" },
    { t: "      reason: ", k: "kw" }, { t: "PodReady\n", k: "str" },
  ], { x: M, y: 4.3, w: 6.5, h: 2.9, fontSize: 11 });

  // RIGHT: phase taxonomy + observability surfaces
  const rx = 7.3;
  s.addText("Phase taxonomy", {
    x: rx, y: 4.3, w: W - rx - M, h: 0.35,
    fontFace: F_DISPLAY, fontSize: 14, bold: true, color: INK, margin: 0,
  });
  const phases = [
    ["Pending",     "controller accepted CR"],
    ["Compiled",    "spec parsed → ConfigMaps written"],
    ["Ready",       "data plane confirmed enforcing"],
    ["Running",     "Pod ready (sandbox only)"],
    ["Degraded",    "valid but partial — see reason"],
    ["Failed",      "hard prereq broken"],
    ["Active",      "grant phase (EgressApproval)"],
    ["Expired",     "grant terminal phase"],
  ];
  const py = 4.7;
  phases.forEach(([p, d], i) => {
    const y = py + i * 0.27;
    s.addText(p, {
      x: rx, y, w: 1.4, h: 0.25,
      fontFace: F_CODE, fontSize: 10, color: ACCENT, margin: 0,
    });
    s.addText(d, {
      x: rx + 1.4, y, w: W - rx - 1.4 - M, h: 0.25,
      fontFace: F_BODY, fontSize: 10, color: INK, margin: 0,
    });
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 9: CRD CATALOG — all 11 kinds (compact reference)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3.6 · architecture · the CRD set");
  title(s, "Eleven CRDs.", { fontSize: 42 });
  lede(s,
    "Nine namespaced (one per sandbox concern) plus two cluster-scoped (tenant-wide " +
    "identity + cross-cluster trust topology).  Every kind has an OpenAPIv3 schema, " +
    "a dedicated reconciler, and ValidatingAdmissionPolicy guard-rails.",
    { y: 2.45, h: 1.3 }
  );

  // Table header
  const cols = { kind: M, scope: M + 1.95, what: M + 2.95, src: M + 9.55 };
  const headY = 4.05;
  const hdrColor = MUTED;
  s.addText("KIND",       { x: cols.kind,  y: headY, w: 1.9, h: 0.28, fontFace: F_BODY, fontSize: 9, color: hdrColor, charSpacing: 3, margin: 0 });
  s.addText("SCOPE",      { x: cols.scope, y: headY, w: 0.95, h: 0.28, fontFace: F_BODY, fontSize: 9, color: hdrColor, charSpacing: 3, margin: 0 });
  s.addText("WHAT IT DOES",{ x: cols.what,  y: headY, w: 6.5, h: 0.28, fontFace: F_BODY, fontSize: 9, color: hdrColor, charSpacing: 3, margin: 0 });
  s.addText("RECONCILER", { x: cols.src,   y: headY, w: 3.0, h: 0.28, fontFace: F_BODY, fontSize: 9, color: hdrColor, charSpacing: 3, margin: 0 });
  // header underline
  s.addShape(pres.shapes.LINE, {
    x: M, y: headY + 0.32, w: W - 2 * M, h: 0,
    line: { color: ACCENT, width: 0.75 },
  });

  const crds = [
    ["KarsSandbox",      "ns",      "provisions a governed sandbox (9 K8s primitives)",       "reconciler/mod.rs"],
    ["InferencePolicy",  "ns",      "model preference · content safety · token budget",        "inference_policy"],
    ["ToolPolicy",       "ns",      "per-tool approval channel · rate limit · audit hooks",    "tool_policy"],
    ["EgressApproval",   "ns",      "time-boxed egress grant (reason + ticket + TTL)",         "egress_approval"],
    ["KarsMemory",       "ns",      "Foundry Memory Store binding · scope + retention",        "kars_memory"],
    ["KarsEval",         "ns",      "conformance run over a sandbox · signed corpus ref",      "kars_eval"],
    ["A2AAgent",         "ns",      "A2A 1.0.0 agent + signed Agent Card · trust roots",       "a2a_agent"],
    ["McpServer",        "ns",      "MCP server registry · OAuth · tool exposure rules",       "mcp_server"],
    ["KarsPairing",      "ns",      "single-use mesh-peer pairing token · TTL + AMID bind",    "pairing"],
    ["KarsAuthConfig",   "cluster", "Entra tenant + authority + sponsor identities",           "auth_config"],
    ["TrustGraph",       "cluster", "cross-cluster trust topology · signed edges (A2A)",       "trust_graph"],
  ];
  const ry0 = 4.3;
  const rh = 0.245;
  crds.forEach(([k, scope, what, src], i) => {
    const y = ry0 + i * rh;
    s.addText(k, {
      x: cols.kind, y, w: 1.9, h: rh,
      fontFace: F_CODE, fontSize: 10, color: INK, margin: 0,
    });
    s.addText(scope, {
      x: cols.scope, y, w: 0.95, h: rh,
      fontFace: F_BODY, fontSize: 10, color: scope === "cluster" ? ACCENT : MUTED, margin: 0,
    });
    s.addText(what, {
      x: cols.what, y, w: 6.5, h: rh,
      fontFace: F_BODY, fontSize: 10, color: INK, margin: 0,
    });
    s.addText(src, {
      x: cols.src, y, w: 3.0, h: rh,
      fontFace: F_CODE, fontSize: 9, color: QUIET, margin: 0,
    });
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 10: InferencePolicy — real CR + prose (was old slide 8)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§4 · policy · InferencePolicy");
  title(s, "Policy is data.", { fontSize: 42 });
  codeBlock(s, [
    { t: "apiVersion: ", k: "kw" }, { t: "kars.azure.com/v1alpha1\n", k: "str" },
    { t: "kind: ", k: "kw" }, { t: "InferencePolicy\n", k: "str" },
    { t: "metadata:\n  name: research-agent-policy\n" },
    { t: "spec:\n" },
    { t: "  appliesTo:\n" },
    { t: "    sandboxName: ", k: "kw" }, { t: "research-agent\n", k: "str" },
    { t: "  modelPreference:\n" },
    { t: "    primary:\n" },
    { t: "      provider: ", k: "kw" }, { t: "azure-openai\n", k: "str" },
    { t: "      deployment: ", k: "kw" }, { t: "gpt-4.1\n", k: "str" },
    { t: "    fallback:\n" },
    { t: "      - provider: ", k: "kw" }, { t: "anthropic\n", k: "str" },
    { t: "        model: ", k: "kw" }, { t: "claude-opus-4.7\n", k: "str" },
    { t: "  contentSafety:\n" },
    { t: "    requirePromptShields: ", k: "kw" }, { t: "true\n", k: "str" },
    { t: "  tokenBudget:\n" },
    { t: "    perRequestTokens: ", k: "kw" }, { t: "32000\n", k: "str" },
    { t: "    perWindow:        ", k: "kw" }, { t: "{ window: 1h, tokens: 200000 }\n", k: "str" },
  ], { x: M, y: 2.4, w: 6.6, h: 4.7, fontSize: 11 });

  // right column — prose
  s.addText("What the controller does", {
    x: 8.1, y: 2.4, w: W - 8.1 - M, h: 0.4,
    fontFace: F_DISPLAY, fontSize: 18, bold: true, color: INK, margin: 0,
  });
  s.addText(
    [
      { text: "Compiles the spec to a router-side policy envelope.  Writes it into the sandbox ConfigMap.  Stamps the digest on the sandbox status. Router refuses to start until digest matches.",
        options: { breakLine: true, paraSpaceAfter: 8 } },
      { text: "On a model call, the router checks: provider available?  budget under limit?  Content Safety green? — then routes (with failover) and writes the audit row.",
        options: { breakLine: true, paraSpaceAfter: 8 } },
      { text: "Edit the CR, re-apply.  Drift surfaces as a Warning Event.  Next call uses the new policy. No restart.",
        options: { } },
    ],
    {
      x: 8.1, y: 2.85, w: W - 8.1 - M, h: 4.2,
      fontFace: F_BODY, fontSize: 12.5, color: INK, margin: 0,
      paraSpaceAfter: 6,
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 11: ToolPolicy + EgressApproval — two more CRDs in action
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§4.1 · policy · two more");
  title(s, "Approval channels.  Time-boxed exceptions.", { fontSize: 28 });
  lede(s,
    "ToolPolicy gates a single tool — approval mode + channel + rate limit. EgressApproval " +
    "overlays a temporary outbound grant that auto-expires.  Both compile to the same " +
    "router envelope as InferencePolicy.",
    { y: 2.35, h: 1.1 }
  );

  // LEFT — ToolPolicy
  s.addText("ToolPolicy", {
    x: M, y: 3.65, w: 6.3, h: 0.4,
    fontFace: F_DISPLAY, fontSize: 16, bold: true, color: INK, margin: 0,
  });
  codeBlock(s, [
    { t: "kind: ", k: "kw" }, { t: "ToolPolicy\n", k: "str" },
    { t: "metadata:\n  name: demo-web-fetch\n" },
    { t: "spec:\n" },
    { t: "  appliesTo:\n    tool: ", k: "kw" }, { t: "\"web.fetch\"\n", k: "str" },
    { t: "    sandboxMatchLabels:\n" },
    { t: "      kars.azure.com/sandbox: ", k: "kw" }, { t: "demo-agent\n", k: "str" },
    { t: "  approval:\n" },
    { t: "    mode: ", k: "kw" },    { t: "always\n", k: "str" },
    { t: "    channel: ", k: "kw" }, { t: "telegram\n", k: "str" },
    { t: "  rateLimit:\n" },
    { t: "    rps: ", k: "kw" }, { t: "1\n", k: "str" },
    { t: "    burst: ", k: "kw" }, { t: "3\n", k: "str" },
    { t: "    window: ", k: "kw" }, { t: "\"1m\"\n", k: "str" },
  ], { x: M, y: 4.05, w: 6.3, h: 3.4, fontSize: 10.5 });

  // RIGHT — EgressApproval
  const rx = 7.0;
  s.addText("EgressApproval", {
    x: rx, y: 3.65, w: W - rx - M, h: 0.4,
    fontFace: F_DISPLAY, fontSize: 16, bold: true, color: INK, margin: 0,
  });
  codeBlock(s, [
    { t: "kind: ", k: "kw" }, { t: "EgressApproval\n", k: "str" },
    { t: "metadata:\n  name: demo-stripe-grant\n" },
    { t: "spec:\n" },
    { t: "  sandbox: ", k: "kw" }, { t: "demo-agent\n", k: "str" },
    { t: "  hosts:\n" },
    { t: "    - host: ", k: "kw" }, { t: "api.stripe.com\n", k: "str" },
    { t: "      port: ", k: "kw" }, { t: "443\n", k: "str" },
    { t: "  ttl: ", k: "kw" }, { t: "PT10M  ", k: "str" }, { t: "# 10-minute grant\n", k: "muted" },
    { t: "  reason: ", k: "kw" }, { t: "|\n", k: "str" },
    { t: "    payment-flow walkthrough for the\n", k: "str" },
    { t: "    weekly demo recording\n", k: "str" },
    { t: "  ticket: ", k: "kw" }, { t: "DEMO-001\n", k: "str" },
  ], { x: rx, y: 4.05, w: W - rx - M, h: 3.4, fontSize: 10.5 });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 12: SANDBOX — Victor style, the pod artefact
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§5 · sandbox · the pod");
  title(s, "The pod is the unit of trust.", { fontSize: 36 });
  lede(s,
    "Two containers + one init container, three users, one network namespace. Every byte " +
    "from UID 1000 (agent) is rewritten to loopback. UID 1001 (router) is the only path " +
    "out.  Init container UID 0 dies after 300 ms; the agent never has root.",
    { y: 2.45, h: 1.5 }
  );

  // single artefact: a labelled pod box
  const px = M + 0.5, py = 4.2, pw = W - 2 * M - 1.0, ph = 2.9;
  s.addShape(pres.shapes.RECTANGLE, {
    x: px, y: py, w: pw, h: ph,
    fill: { color: PAPER }, line: { color: ACCENT, width: 1.2 },
  });
  s.addText("Pod  ·  kars-research-agent / research-agent-7d4cb…", {
    x: px + 0.2, y: py + 0.1, w: pw - 0.4, h: 0.3,
    fontFace: F_CODE, fontSize: 11, color: MUTED, margin: 0,
  });

  // three rows = three containers
  const rows = [
    ["init:egress-guard",  "UID 0  ·  6 iptables rules  ·  exits after 300ms",                  "readOnlyRootFilesystem: false (just init)"],
    ["openclaw",           "UID 1000  ·  the agent process  ·  loopback-only outbound",         "seccomp: kars-strict  ·  drop ALL caps"],
    ["inference-router",   "UID 1001  ·  axum  ·  :8443 admin · :8444 transparent proxy",       "the only path to the outside world"],
  ];
  const ry = py + 0.55;
  const rh = (ph - 0.7) / 3;
  rows.forEach(([name, body, foot], i) => {
    const y = ry + i * rh;
    if (i > 0) {
      s.addShape(pres.shapes.LINE, {
        x: px + 0.2, y, w: pw - 0.4, h: 0,
        line: { color: "E1E4E8", width: 0.5 },
      });
    }
    s.addText(name, {
      x: px + 0.2, y: y + 0.1, w: 2.5, h: 0.3,
      fontFace: F_DISPLAY, fontSize: 13, bold: true, color: INK, margin: 0,
    });
    s.addText(body, {
      x: px + 2.7, y: y + 0.1, w: pw - 3.0, h: 0.3,
      fontFace: F_BODY, fontSize: 11.5, color: INK, margin: 0,
    });
    s.addText(foot, {
      x: px + 2.7, y: y + 0.4, w: pw - 3.0, h: 0.3,
      fontFace: F_CODE, fontSize: 10, color: MUTED, margin: 0,
    });
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 13: SANDBOX — the iptables that makes it true
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§5.1 · sandbox · the gate");
  title(s, "Six iptables rules.", { fontSize: 44 });
  codeBlock(s,
    "iptables -t nat   -A OUTPUT -m owner --uid-owner 1000 -d 127.0.0.1 -j RETURN\n" +
    "iptables -t nat   -A OUTPUT -m owner --uid-owner 1000 -p udp --dport 53 -j RETURN\n" +
    "iptables          -A OUTPUT -m owner --uid-owner 1000 -d 127.0.0.0/8 -j ACCEPT\n" +
    "iptables          -A OUTPUT -m owner --uid-owner 1000 -p udp --dport 53 -j ACCEPT\n" +
    "iptables          -A OUTPUT -m owner --uid-owner 1000 -j REJECT\n" +
    "iptables -t nat   -A OUTPUT -m owner --uid-owner 1000 -p tcp --dport 80  -j REDIRECT --to-port 8444\n" +
    "iptables -t nat   -A OUTPUT -m owner --uid-owner 1000 -p tcp --dport 443 -j REDIRECT --to-port 8444\n",
    { x: M, y: 3.0, w: 8.4, h: 3.4, fontSize: 9.5 });

  const rx = 9.0;
  s.addText("Reading the rules", {
    x: rx, y: 3.0, w: W - rx - M, h: 0.4,
    fontFace: F_DISPLAY, fontSize: 16, bold: true, color: INK, margin: 0,
  });
  s.addText(
    [
      { text: "UID 1000 is the agent.  UID 1001 is the inference-router sidecar — distinct user, so the rules apply only to agent traffic.",
        options: { breakLine: true, paraSpaceAfter: 8 } },
      { text: "Line 5 fails closed:  any outbound the agent didn't get through loopback or DNS gets rejected.",
        options: { breakLine: true, paraSpaceAfter: 8 } },
      { text: "Lines 6 + 7 are why the agent can still talk to the world:  :80/:443 are NAT-redirected to the router's transparent proxy on :8444, where every call is policy-checked and audited.",
        options: { } },
    ],
    {
      x: rx, y: 3.45, w: W - rx - M, h: 3.5,
      fontFace: F_BODY, fontSize: 12, color: INK, margin: 0,
      paraSpaceAfter: 6,
    });
  s.addText("controller/src/reconciler/mod.rs:1916-1958", {
    x: M, y: 6.7, w: W - 2 * M, h: 0.3,
    fontFace: F_CODE, fontSize: 10, color: QUIET, align: "left", margin: 0,
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 15: NETWORK EGRESS — Learn vs Strict + signed OCI allowlist pipeline
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§5.2 · sandbox · the allowlist");
  title(s, "Egress:  learn it, sign it, enforce it.", { fontSize: 32 });
  lede(s,
    "Two modes shipped, one more on deck.  Learn mode records every host the agent " +
    "reaches — the operator turns that into a signed OCI bundle that the cluster pulls, " +
    "cosign-verifies, and the router atomically hot-reloads as the L7 allowlist.",
    { y: 2.45, h: 1.4 }
  );

  // ── TOP HALF: Learn vs Strict (two panels)
  const ph = 1.6;
  const py = 4.0;
  const pwHalf = (W - 2 * M - 0.4) / 2;

  // LEFT panel — Learn
  s.addShape(pres.shapes.RECTANGLE, {
    x: M, y: py, w: pwHalf, h: ph,
    fill: { color: PAPER }, line: { color: ACCENT, width: 1 },
  });
  s.addText("Learn", {
    x: M + 0.2, y: py + 0.12, w: 2, h: 0.35,
    fontFace: F_DISPLAY, fontSize: 16, bold: true, color: INK, margin: 0,
  });
  s.addText("default", {
    x: M + 1.4, y: py + 0.18, w: 1.0, h: 0.25,
    fontFace: F_CODE, fontSize: 10, color: ACCENT, margin: 0,
  });
  s.addText(
    "Every host the agent reaches is logged and folded into the next allowlist proposal. " +
    "Blocklist still applied first. Discovery without a deploy step.",
    {
      x: M + 0.2, y: py + 0.5, w: pwHalf - 0.4, h: ph - 0.55,
      fontFace: F_BODY, fontSize: 11, color: INK, margin: 0,
    });

  // RIGHT panel — Strict
  const px2 = M + pwHalf + 0.4;
  s.addShape(pres.shapes.RECTANGLE, {
    x: px2, y: py, w: pwHalf, h: ph,
    fill: { color: PAPER }, line: { color: ACCENT, width: 1 },
  });
  s.addText("Strict", {
    x: px2 + 0.2, y: py + 0.12, w: 2, h: 0.35,
    fontFace: F_DISPLAY, fontSize: 16, bold: true, color: INK, margin: 0,
  });
  s.addText("production", {
    x: px2 + 1.4, y: py + 0.18, w: 1.4, h: 0.25,
    fontFace: F_CODE, fontSize: 10, color: ACCENT, margin: 0,
  });
  s.addText(
    "Anything outside the signed allowlist gets a 4xx with a precise error. Operator " +
    "can layer time-boxed EgressApproval grants on top. Fails closed on verify failure.",
    {
      x: px2 + 0.2, y: py + 0.5, w: pwHalf - 0.4, h: ph - 0.55,
      fontFace: F_BODY, fontSize: 11, color: INK, margin: 0,
    });

  // ── BOTTOM HALF: The signed allowlist pipeline
  const fy = py + ph + 0.4;
  s.addText("Signed OCI allowlist  ·  the pipeline", {
    x: M, y: fy, w: W - 2 * M, h: 0.3,
    fontFace: F_DISPLAY, fontSize: 13, bold: true, color: INK, charSpacing: 2, margin: 0,
  });

  // 5-stage pipeline as connected chips
  const stages = [
    "kars egress  --sign",
    "OCI artifact  (ACR / ghcr)",
    "cosign verify  (Fulcio + SAN)",
    "ConfigMap  +  digest",
    "router  ·  L7 hot-reload",
  ];
  const sy = fy + 0.45;
  const sh = 0.55;
  const sgap = 0.12;
  const stotal = W - 2 * M;
  const sw = (stotal - (stages.length - 1) * sgap) / stages.length;
  stages.forEach((stage, i) => {
    const x = M + i * (sw + sgap);
    s.addShape(pres.shapes.ROUNDED_RECTANGLE, {
      x, y: sy, w: sw, h: sh,
      rectRadius: 0.06,
      fill: { color: ACCENT_LIGHT }, line: { color: ACCENT, width: 0.75 },
    });
    s.addText(stage, {
      x, y: sy, w: sw, h: sh,
      fontFace: F_CODE, fontSize: 10, color: INK,
      align: "center", valign: "middle", margin: 0,
    });
    // arrow between stages
    if (i < stages.length - 1) {
      const ax = x + sw + 0.005;
      s.addShape(pres.shapes.LINE, {
        x: ax, y: sy + sh / 2, w: sgap - 0.01, h: 0,
        line: { color: ACCENT, width: 1, endArrowType: "triangle" },
      });
    }
  });

  // Source line at very bottom
  s.addText("controller/src/policy_fetcher.rs  ·  egress_allowlist_compile.rs  ·  inference-router/src/egress_allowlist_loader.rs", {
    x: M, y: sy + sh + 0.15, w: W - 2 * M, h: 0.25,
    fontFace: F_CODE, fontSize: 8.5, color: QUIET, align: "left", margin: 0,
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 16: MESH — Victor style, one named artefact (the KNOCK frame itself)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§6 · mesh");
  title(s, "Agents authenticate each other.", { fontSize: 42 });
  lede(s,
    "Every inter-agent message rides Signal Protocol:  Ed25519 + X25519 identities, X3DH for " +
    "session establishment, Double Ratchet for forward-secret message keys. The relay routes " +
    "opaque bytes and never sees plaintext.",
    { y: 2.5, h: 1.4 }
  );

  codeBlock(s, [
    { t: "{\n" },
    { t: "  \"v\": ", k: "kw" }, { t: "1,\n", k: "str" },
    { t: "  \"type\": ", k: "kw" }, { t: "\"knock\",\n", k: "str" },
    { t: "  \"from\": ", k: "kw" }, { t: "\"did:mesh:7f3a…\",\n", k: "str" },
    { t: "  \"to\":   ", k: "kw" }, { t: "\"did:mesh:b210…\",\n", k: "str" },
    { t: "  \"id\":   ", k: "kw" }, { t: "\"k-9c0d7e\",\n", k: "str" },
    { t: "  \"ts\":   ", k: "kw" }, { t: "\"2026-06-08T14:23:11Z\",\n", k: "str" },
    { t: "  \"intent\": ", k: "kw" }, { t: "\"tool.invoke\",\n", k: "str" },
    { t: "  \"establishment\": {\n" },
    { t: "    \"ik\": ", k: "kw" }, { t: "\"…\",  ", k: "str" }, { t: "// X25519 identity key\n", k: "muted" },
    { t: "    \"ek\": ", k: "kw" }, { t: "\"…\",  ", k: "str" }, { t: "// ephemeral key\n", k: "muted" },
    { t: "    \"spk_id\": ", k: "kw" }, { t: "42,   ", k: "str" }, { t: "// signed prekey id\n", k: "muted" },
    { t: "    \"otk_id\": ", k: "kw" }, { t: "117   ", k: "str" }, { t: "// one-time prekey id\n", k: "muted" },
    { t: "  }\n}\n" },
  ], { x: M, y: 3.85, w: 6.6, h: 3.55, fontSize: 11 });

  s.addText("The KNOCK frame", {
    x: 8.1, y: 3.85, w: W - 8.1 - M, h: 0.4,
    fontFace: F_DISPLAY, fontSize: 18, bold: true, color: INK, margin: 0,
  });
  s.addText(
    [
      { text: "Carries the initiator's X3DH establishment.  Receiver's policy hook gates accept BEFORE the session opens.",
        options: { breakLine: true } },
      { text: " " , options: { breakLine: true, fontSize: 6 } },
      { text: "Once accepted, all subsequent frames use Double Ratchet keys.  Compromising one frame's key does not compromise past frames.",
        options: {} },
    ],
    {
      x: 8.1, y: 4.35, w: W - 8.1 - M, h: 3.0,
      fontFace: F_BODY, fontSize: 13, color: INK, margin: 0,
      paraSpaceAfter: 6,
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 15: GOVERNANCE — the four layers (annotated stack)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§7 · governance");
  title(s, "Four layers.  Each enforced once, by one component.", { fontSize: 26 });
  lede(s,
    "Defence-in-depth — but each layer is owned by exactly one component, so failures " +
    "have a single name.  No double-checking, no contradictory decisions.",
    { y: 2.4, h: 1.1 }
  );

  const layers = [
    ["1", "Provider",       "Azure OpenAI · Anthropic · OpenAI · Bedrock — RBAC + region + tenant", "external"],
    ["2", "Router",         "InferencePolicy · ToolPolicy · EgressApproval · Content Safety",       "inference-router/src/failover.rs:51-95"],
    ["3", "Pod",            "kars-strict seccomp · drop ALL caps · readOnlyRootFilesystem",         "deploy/helm/kars/files/kars-strict.json"],
    ["4", "Cluster",        "default-deny NetworkPolicy · admission policies · pairing tokens",     "controller/src/reconciler/mod.rs"],
  ];
  const ly0 = 3.7;
  const lh = 0.85;
  layers.forEach(([n, name, body, src], i) => {
    const y = ly0 + i * lh;
    s.addText(n, {
      x: M, y, w: 0.4, h: lh,
      fontFace: F_CODE, fontSize: 18, bold: true, color: ACCENT, margin: 0, valign: "top",
    });
    s.addText(name, {
      x: M + 0.55, y, w: 1.7, h: 0.4,
      fontFace: F_DISPLAY, fontSize: 16, bold: true, color: INK, margin: 0, valign: "top",
    });
    s.addText(body, {
      x: M + 2.3, y, w: 7.0, h: 0.4,
      fontFace: F_BODY, fontSize: 13, color: INK, margin: 0, valign: "top",
    });
    s.addText(src, {
      x: M + 2.3, y: y + 0.35, w: 7.0, h: 0.3,
      fontFace: F_CODE, fontSize: 10, color: QUIET, margin: 0, valign: "top",
    });
    if (i < layers.length - 1) {
      s.addShape(pres.shapes.LINE, {
        x: M, y: y + lh - 0.05, w: W - 2 * M, h: 0,
        line: { color: "E1E4E8", width: 0.5 },
      });
    }
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 16: BLUEPRINTS — six shapes, real labels per shape
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§8 · patterns · blueprints");
  title(s, "Six shapes you can stamp.", { fontSize: 38 });
  lede(s,
    "Each blueprint is a documented composition — CRD set + helm overlay + runbook — for " +
    "a concrete operational pattern.  Pick one, adapt, ship.",
    { y: 2.5, h: 1.0 }
  );

  // 3x2 tiles
  const blueprints = [
    ["01",  "Governed Sandbox",    "single agent · default-deny egress · audit",          "docs/blueprints/01"],
    ["02",  "Multi-Runtime Mesh",  "OpenClaw + Hermes + Anthropic sharing one mesh",      "docs/blueprints/02"],
    ["03",  "A2A Bridge",          "cross-cluster federation · mTLS + signed Agent Card", "docs/blueprints/03"],
    ["04",  "Private Model + ACL", "BYO inference endpoint · signed allowlist",           "docs/blueprints/04"],
    ["05",  "Approval Channels",   "Telegram / Slack / Discord per-tool gates",           "docs/blueprints/05"],
    ["06",  "Sovereign / Air-gap", "Kata + SEV-SNP · attestation-gated · zero-egress",    "docs/blueprints/06"],
  ];
  const tw = 4.0, th = 1.4, gap = 0.2;
  const cols = 3;
  const totalW = cols * tw + (cols - 1) * gap;
  const x0 = (W - totalW) / 2;
  const y0 = 4.0;
  blueprints.forEach(([n, name, sub, ref], i) => {
    const col = i % cols, row = Math.floor(i / cols);
    const x = x0 + col * (tw + gap);
    const y = y0 + row * (th + gap);
    s.addShape(pres.shapes.RECTANGLE, {
      x, y, w: tw, h: th,
      fill: { color: PAPER }, line: { color: ACCENT, width: 1 },
    });
    s.addText(n, {
      x: x + 0.2, y: y + 0.15, w: 0.6, h: 0.4,
      fontFace: F_CODE, fontSize: 16, bold: true, color: ACCENT, margin: 0,
    });
    s.addText(name, {
      x: x + 0.85, y: y + 0.15, w: tw - 1.05, h: 0.4,
      fontFace: F_DISPLAY, fontSize: 14, bold: true, color: INK, margin: 0,
    });
    s.addText(sub, {
      x: x + 0.2, y: y + 0.6, w: tw - 0.4, h: 0.5,
      fontFace: F_BODY, fontSize: 11, color: INK, margin: 0,
    });
    s.addText(ref, {
      x: x + 0.2, y: y + th - 0.35, w: tw - 0.4, h: 0.25,
      fontFace: F_CODE, fontSize: 9, color: QUIET, margin: 0,
    });
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 17: MULTI-RUNTIME — 8 wired runtimes
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§8.1 · patterns · runtimes");
  title(s, "Eight wired.  Bring your own for the ninth.", { fontSize: 32 });
  lede(s,
    "WIRED_KINDS in cli/src/runtime.ts is the single source of truth.  Adding a new runtime: " +
    "new image, new entrypoint, new controller branch, new CRD variant — call it a few days, not a few weeks.",
    { y: 2.45, h: 1.1 }
  );
  const rts = [
    ["OpenClaw", "TypeScript · 24 plugin tools · channels"],
    ["Hermes", "Python 3.11+ · 15 plugin tools · 20+ Hermes channels"],
    ["Anthropic", "Claude SDK · Python · base_url loopback"],
    ["MAF", "Microsoft Agent Framework · Python wired today"],
    ["LangGraph py", "Python · graph-orchestrated"],
    ["LangGraph ts", "TypeScript · graph-orchestrated"],
    ["Pydantic AI", "Python · typed-tool-call DSL"],
    ["OpenAI Agents", "Official OpenAI Agents SDK"],
  ];
  const tw = 2.9, th = 1.0, gap = 0.15;
  const cols = 4;
  const totalW = cols * tw + (cols - 1) * gap;
  const x0 = (W - totalW) / 2;
  const y0 = 4.0;
  rts.forEach(([name, sub], i) => {
    const col = i % cols, row = Math.floor(i / cols);
    const x = x0 + col * (tw + gap);
    const y = y0 + row * (th + gap);
    s.addShape(pres.shapes.RECTANGLE, {
      x, y, w: tw, h: th,
      fill: { color: PAPER }, line: { color: ACCENT, width: 1 },
    });
    s.addText(name, {
      x: x + 0.2, y: y + 0.15, w: tw - 0.4, h: 0.4,
      fontFace: F_DISPLAY, fontSize: 15, bold: true, color: INK, margin: 0,
    });
    s.addText(sub, {
      x: x + 0.2, y: y + 0.55, w: tw - 0.4, h: 0.4,
      fontFace: F_BODY, fontSize: 11, color: MUTED, margin: 0,
    });
  });
  s.addText("cli/src/runtime.ts · controller/src/reconciler/runtime.rs", {
    x: M, y: 7.0, w: W - 2 * M, h: 0.3,
    fontFace: F_CODE, fontSize: 10, color: QUIET, align: "left", margin: 0,
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 18: BUILT ON AGT (statement + named contributions)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§9 · upstream");
  title(s, "Built on the Microsoft Agent Governance Toolkit.", { fontSize: 30 });
  lede(s,
    "AGT ships the protocol and the libraries.  kars adds the Kubernetes-native runtime, the " +
    "per-sandbox governance data plane, and the operator-facing UX.  Patches flow back upstream — " +
    "we ship from a pinned branch (vendor/agt/pin.json) so the wire format stays consistent edge-to-edge.",
    { y: 2.5, h: 1.6 }
  );
  primitiveRow(s, [
    ["PR #2772", "Proof-of-possession on /ws connect frames"],
    ["pending PR", "X3DH KDF spec compliance"],
    ["landed", "Multiple Python MeshClient compat fixes"],
    ["test corpus", "Cross-runtime wire-format byte equivalence"],
  ], { y: 5.0 });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 19: WHAT'S NEXT — outcome-shaped (capability → outcome → proof)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§10 · next");
  title(s, "What becomes possible.", { fontSize: 42 });
  lede(s,
    "Four shipping targets.  Each one tightens the loop between what the operator declares " +
    "and what the cluster delivers.",
    { y: 2.45, h: 1.1 }
  );

  // Four rows — outcome-shaped: capability shipping → what becomes possible → proof
  const next = [
    ["Hermes Act 2",     "Every runtime ships with mesh on day one",         "Python MeshClient at TS SDK parity · same KNOCK · same Double Ratchet"],
    ["kars-sre",         "The cluster diagnoses itself before you do",       "In-cluster SRE agent · 5 read-only tools · 2 approval-gated fixes"],
    ["Sovereign GA",     "Regulated environments deploy with one apply",     "Blueprint 06 from compose-by-hand to one-command bundle"],
    ["Attestation",      "Workloads run only on hardware they trust",        "Kata + SEV-SNP attestation-gated KarsSandbox · evidence in .status"],
  ];
  const ny0 = 3.7;
  const nh = 0.78;
  next.forEach(([cap, outcome, proof], i) => {
    const y = ny0 + i * nh;
    s.addText(cap, {
      x: M, y, w: 2.3, h: 0.35,
      fontFace: F_CODE, fontSize: 13, color: ACCENT, margin: 0, valign: "top",
    });
    s.addText(outcome, {
      x: M + 2.4, y, w: W - M - 2.4 - M, h: 0.35,
      fontFace: F_DISPLAY, fontSize: 15, bold: true, color: INK, margin: 0, valign: "top",
    });
    s.addText(proof, {
      x: M + 2.4, y: y + 0.35, w: W - M - 2.4 - M, h: 0.35,
      fontFace: F_BODY, fontSize: 11.5, color: MUTED, margin: 0, valign: "top",
    });
    if (i < next.length - 1) {
      s.addShape(pres.shapes.LINE, {
        x: M, y: y + nh - 0.05, w: W - 2 * M, h: 0,
        line: { color: "E1E4E8", width: 0.5 },
      });
    }
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 20: TRY IT (Stripe-docs style: command block + real output)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§11 · try it");
  title(s, "kars dev", { fontSize: 80 });
  codeBlock(s, [
    { t: "$ ", k: "muted" }, { t: "git clone https://github.com/Azure/kars\n" },
    { t: "$ ", k: "muted" }, { t: "cd kars/cli && npm ci && npm run build && npm link\n" },
    { t: "$ ", k: "muted" }, { t: "kars dev\n", k: "kw" },
    { t: "\n" },
    { t: "  ✓ kind cluster ready\n", k: "str" },
    { t: "  ✓ AGT toolkit cloned + wheels built\n", k: "str" },
    { t: "  ✓ helm chart applied  (controller + relay + registry)\n", k: "str" },
    { t: "  ✓ runtime image loaded into kind\n", k: "str" },
    { t: "  ✓ sandbox 'agent-1' Running 2/2\n", k: "str" },
    { t: "\n" },
    { t: "$ ", k: "muted" }, { t: "kars connect agent-1     ", k: "kw" }, { t: "# WebUI on http://localhost:18789", k: "muted" },
  ], { x: M, y: 3.9, w: W - 2 * M, h: 3.0, fontSize: 13 });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 21: ACT I — Secure runtime, in motion (live demo agenda card)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§12 · live demo · act i");
  title(s, "Secure runtime, in motion.", { fontSize: 38 });
  lede(s,
    "A normal agent on the cluster.  Watch a governed research-agent come up, " +
    "answer a question, get its tool call approval-gated via Telegram, and land " +
    "every step in the audit log.",
    { y: 2.5, h: 1.3 }
  );

  // Five-step timeline — each step a row: number + command/event + outcome
  const steps = [
    ["1", "kars dev",
      "kind cluster up · controller / relay / registry installed · ready in ~60 s"],
    ["2", "kubectl get karssandbox",
      "nothing yet — the cluster is healthy but unused"],
    ["3", "kars add research-agent --blueprint governed",
      "KarsSandbox CR applied · Namespace + NetworkPolicy + Deployment created · pod Running 2/2"],
    ["4", "kars connect research-agent",
      "WebUI on :18789 · ask:  \"summarise github.com/Azure/kars\""],
    ["5", "tool web.fetch is gated",
      "ToolPolicy → Telegram approval prompt → approve → fetch → answer + audit row in /audit/events"],
  ];
  const sy = 3.95;
  const sh = 0.6;
  steps.forEach(([n, cmd, outcome], i) => {
    const y = sy + i * sh;
    s.addText(n, {
      x: M, y, w: 0.45, h: sh,
      fontFace: F_CODE, fontSize: 18, bold: true, color: ACCENT, margin: 0, valign: "top",
    });
    s.addText(cmd, {
      x: M + 0.55, y, w: 5.4, h: 0.32,
      fontFace: F_CODE, fontSize: 12, color: INK, margin: 0,
    });
    s.addText(outcome, {
      x: M + 0.55, y: y + 0.3, w: W - M - 0.55 - M, h: 0.32,
      fontFace: F_BODY, fontSize: 11, color: MUTED, margin: 0,
    });
    if (i < steps.length - 1) {
      s.addShape(pres.shapes.LINE, {
        x: M, y: y + sh - 0.04, w: W - 2 * M, h: 0,
        line: { color: "E1E4E8", width: 0.5 },
      });
    }
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 22: INTERMISSION — same cluster, second agent (dark, like a chapter break)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = dark();
  page++;
  eyebrow(s, "§12.1 · between acts", QUIET);
  s.addText("Same cluster.  Second agent.", {
    x: M, y: 1.6, w: W - 2 * M, h: 1.6,
    fontFace: F_DISPLAY, fontSize: 56, bold: true, color: PAPER,
    align: "left", valign: "top", margin: 0,
  });
  s.addText(
    "If you can run a research agent on kars, you can run an SRE one.  Same isolation, " +
    "same policy plane, same audit log — just different tools.",
    {
      x: M, y: 3.6, w: W - 2 * M, h: 1.6,
      fontFace: F_DISPLAY, fontSize: 26, color: ACCENT_LIGHT,
      align: "left", valign: "top", margin: 0,
      paraSpaceAfter: 8,
    });
  s.addText("And — with your approval — it can fix things for you.", {
    x: M, y: 5.6, w: W - 2 * M, h: 0.6,
    fontFace: F_BODY, fontSize: 18, color: QUIET, align: "left", margin: 0,
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 23: ACT II — kars-sre on the same cluster (live demo agenda card)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§12.2 · live demo · act ii");
  title(s, "The cluster fixes itself.  With your nod.", { fontSize: 32 });
  lede(s,
    "Drop an SRE agent onto the same cluster.  Roll out a workload with a classic " +
    "Kubernetes mistake.  Watch the agent ping Telegram, diagnose the cause, propose " +
    "the fix, wait for the operator's approval — then apply it and confirm recovery.",
    { y: 2.45, h: 1.5 }
  );

  const steps = [
    ["1", "kars install sre",
      "kars-sre sandbox up · read-only kubectl tools (get / describe / logs / top / events) · 2 approval-gated mutators (patch / set image)"],
    ["2", "kubectl apply -f webshop.yaml",
      "deployment 'webshop' in ns 'webshop' · image: nginx:1.27-typo  ←  pods stuck ImagePullBackOff"],
    ["3", "telegram ping  ◀  kars-sre",
      "\"webshop/webshop  3/3 pods ImagePullBackOff for 90 s.  Image 'nginx:1.27-typo' not found.  Closest in-use tag in the cluster:  nginx:1.27.3 (4 pods).\""],
    ["4", "kars connect sre  →  \"give me the health overview\"",
      "1 unhealthy workload · proposed fix:  kubectl set image deploy/webshop web=nginx:1.27.3  ·  awaiting approval"],
    ["5", "approve via telegram  →  patch applied",
      "rollout completes · pods Running 3/3 · audit row written · 🟢"],
  ];
  const sy = 4.05;
  const sh = 0.55;
  steps.forEach(([n, cmd, outcome], i) => {
    const y = sy + i * sh;
    s.addText(n, {
      x: M, y, w: 0.45, h: sh,
      fontFace: F_CODE, fontSize: 18, bold: true, color: ACCENT, margin: 0, valign: "top",
    });
    s.addText(cmd, {
      x: M + 0.55, y, w: W - M - 0.55 - M, h: 0.3,
      fontFace: F_CODE, fontSize: 12, color: INK, margin: 0,
    });
    s.addText(outcome, {
      x: M + 0.55, y: y + 0.28, w: W - M - 0.55 - M, h: 0.3,
      fontFace: F_BODY, fontSize: 10.5, color: MUTED, margin: 0,
    });
    if (i < steps.length - 1) {
      s.addShape(pres.shapes.LINE, {
        x: M, y: y + sh - 0.04, w: W - 2 * M, h: 0,
        line: { color: "E1E4E8", width: 0.5 },
      });
    }
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// SLIDE 24: CLOSE (dark, big mark, three-word tagline)
// ─────────────────────────────────────────────────────────────────────────────
{
  const s = dark();
  page++;
  s.addText("kars", {
    x: M, y: 2.4, w: W - 2 * M, h: 2.4,
    fontFace: F_DISPLAY, fontSize: 168, bold: true, color: PAPER,
    align: "left", margin: 0,
  });
  s.addText("Agents.  Production.  Kubernetes.", {
    x: M, y: 5.0, w: W - 2 * M, h: 0.5,
    fontFace: F_BODY, fontSize: 22, color: ACCENT_LIGHT, align: "left", margin: 0,
  });
  s.addText("github.com/Azure/kars", {
    x: M, y: 5.6, w: W - 2 * M, h: 0.4,
    fontFace: F_CODE, fontSize: 14, color: QUIET, align: "left", margin: 0,
  });
}

pres.writeFile({
  fileName: "/Users/pallakatos/Private/Repos/azureclaw/azureclaw/docs/showcase/deliverables/kars-pitch-deck.pptx",
}).then((f) => console.log("wrote:", f));
