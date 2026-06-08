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

// slide 1: TITLE (dark, magazine-style)
let page = 1;
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

// slide 2: THE RACE (Stripe Press lede + four named contenders)
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§1 · the race");
  title(s, "Every cloud is shipping an agent runtime.");
  lede(s,
    "Bedrock Agents, Vertex Agents, OpenAI Assistants, LangChain Cloud, CrewAI. " +
    "They all ship the agent loop, tool calling, and some memory. None ship all four of " +
    "the things that matter for production:  Kubernetes-native deployment on your cluster, " +
    "end-to-end encrypted inter-agent mesh, multiple agent frameworks living together, and " +
    "governance enforced at every byte of egress.",
    { y: 2.8, w: W - 2 * M, h: 1.8 }
  );
  primitiveRow(s, [
    ["Bedrock Agents", "managed · locked to Bedrock-hosted models"],
    ["Vertex Agents", "managed · locked to Vertex-hosted models"],
    ["OpenAI Assistants", "managed · single provider · no on-prem"],
    ["LangGraph Cloud", "managed orchestration · no isolation primitive"],
  ], { y: 5.0 });
}

// slide 3: WHAT KARS IS (dark, statement slide, eyebrow + body)
{
  const s = dark();
  page++;
  eyebrow(s, "§2 · what kars is", QUIET);
  s.addText("Secure, multi-runtime AI agent runtime on Azure Kubernetes Service.",
    {
      x: M, y: 1.5, w: W - 2 * M, h: 1.8,
      fontFace: F_DISPLAY, fontSize: 38, bold: true, color: PAPER,
      align: "left", valign: "top", margin: 0,
    });
  s.addText("End-to-end encrypted inter-agent mesh. Governance enforced in the per-sandbox data plane.",
    {
      x: M, y: 3.6, w: W - 2 * M, h: 1.4,
      fontFace: F_DISPLAY, fontSize: 32, bold: false, color: ACCENT_LIGHT,
      align: "left", valign: "top", margin: 0,
    });
  s.addText("Eight wired runtimes today.  Bring-your-own for the ninth.",
    {
      x: M, y: 5.4, w: W - 2 * M, h: 0.5,
      fontFace: F_BODY, fontSize: 18, color: QUIET, align: "left", margin: 0,
    });
}

// slide 4: FOUR PILLARS (overview, Stripe-press style — heading + lede + 4 named primitives)
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3 · architecture");
  title(s, "Four pillars.");
  lede(s,
    "Each pillar is a concrete, auditable subsystem with a named contract, a single source of " +
    "truth in the controller, and observability hooks. The next four sections drill into each.",
    { y: 2.6, h: 1.4 }
  );
  primitiveRow(s, [
    ["sandbox/", "kars-strict seccomp · iptables egress-guard · drop ALL caps"],
    ["agentmesh/", "Signal Protocol · X3DH · Double Ratchet · KNOCK"],
    ["router/", "InferencePolicy · ToolPolicy · Content Safety · budgets"],
    ["contract/v1", "KARS_MODEL · KARS_RUNTIME_KIND · 127.0.0.1:8443"],
  ], { y: 4.6 });
}

// slide 5: SANDBOX — Victor style, one named artefact (the pod itself)
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3.1 · sandbox");
  title(s, "One pod.  Three layers.  Drop all the rest.");
  // Pod outline
  const px = M, py = 2.5, pw = W - 2 * M, ph = 3.8;
  s.addShape(pres.shapes.RECTANGLE, {
    x: px, y: py, w: pw, h: ph,
    fill: { color: PAPER }, line: { color: QUIET, width: 1, dashType: "dash" },
  });
  s.addText("Pod", {
    x: px + 0.2, y: py + 0.15, w: 1, h: 0.3,
    fontFace: F_CODE, fontSize: 10, color: QUIET, margin: 0,
  });
  // init container
  s.addShape(pres.shapes.RECTANGLE, {
    x: px + 0.4, y: py + 0.6, w: pw - 0.8, h: 0.7,
    fill: { color: ACCENT_LIGHT }, line: { color: ACCENT, width: 1 },
  });
  s.addText([
    { text: "init  ", options: { color: ACCENT, fontFace: F_CODE, bold: true } },
    { text: "egress-guard  ", options: { color: INK, fontFace: F_CODE } },
    { text: "iptables: UID 1000 → loopback + DNS only", options: { color: MUTED, fontSize: 12 } },
  ], {
    x: px + 0.6, y: py + 0.6, w: pw - 1.2, h: 0.7,
    fontFace: F_BODY, fontSize: 14, valign: "middle", margin: 0,
  });
  // agent container
  const cw = (pw - 1.0) / 2, ch = 2.0;
  s.addShape(pres.shapes.RECTANGLE, {
    x: px + 0.4, y: py + 1.5, w: cw, h: ch,
    fill: { color: PAPER }, line: { color: ACCENT, width: 1.5 },
  });
  s.addText("agent", {
    x: px + 0.6, y: py + 1.7, w: cw, h: 0.5,
    fontFace: F_CODE, fontSize: 16, bold: true, color: ACCENT, margin: 0,
  });
  s.addText("UID 1000\nreadOnlyRootFilesystem\nrunAsNonRoot\ncapabilities: drop ALL", {
    x: px + 0.6, y: py + 2.25, w: cw - 0.4, h: 1.2,
    fontFace: F_CODE, fontSize: 11, color: INK, margin: 0,
  });
  // router container
  s.addShape(pres.shapes.RECTANGLE, {
    x: px + 0.6 + cw, y: py + 1.5, w: cw, h: ch,
    fill: { color: PAPER }, line: { color: ACCENT, width: 1.5 },
  });
  s.addText("inference-router  (sidecar)", {
    x: px + 0.8 + cw, y: py + 1.7, w: cw, h: 0.5,
    fontFace: F_CODE, fontSize: 16, bold: true, color: ACCENT, margin: 0,
  });
  s.addText("UID 1001\nLLM proxy + egress proxy\nInferencePolicy + ToolPolicy\nContent Safety + audit JSONL", {
    x: px + 0.8 + cw, y: py + 2.25, w: cw - 0.4, h: 1.2,
    fontFace: F_CODE, fontSize: 11, color: INK, margin: 0,
  });
  // Caption row underneath
  s.addText("The agent has exactly one network destination it can reach:  127.0.0.1:8443.", {
    x: M, y: 6.5, w: W - 2 * M, h: 0.5,
    fontFace: F_BODY, fontSize: 14, color: MUTED, margin: 0,
  });
}

// slide 6: SANDBOX — the iptables that makes it true (Stripe-docs style: code + prose)
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3.1 · sandbox · the gate");
  title(s, "Six iptables rules.", { fontSize: 42 });
  codeBlock(s, [
    { t: "# init container, runs once as UID 0\n", k: "muted", br: true },
    { t: "iptables -A OUTPUT -m owner --uid-owner 1000 -o lo -j ACCEPT\n" },
    { t: "iptables -A OUTPUT -m owner --uid-owner 1000 -p udp --dport 53 -j ACCEPT\n" },
    { t: "iptables -A OUTPUT -m owner --uid-owner 1000 -p tcp --dport 53 -j ACCEPT\n" },
    { t: "iptables -A OUTPUT -m owner --uid-owner 1000 -m conntrack \\\n        --ctstate ESTABLISHED,RELATED -j ACCEPT\n" },
    { t: "iptables -A OUTPUT -m owner --uid-owner 1000 -j DROP\n", k: "kw" },
    { t: "iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo \\\n        -p tcp --dport 80  -j REDIRECT --to-port 8444\n" },
    { t: "iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo \\\n        -p tcp --dport 443 -j REDIRECT --to-port 8444\n" },
  ], { x: M, y: 2.6, w: 7.0, h: 4.0, fontSize: 12 });
  rightProse(s, [
    "UID 1000 is the agent.  UID 1001 is the inference-router sidecar — distinct user, so the rules apply only to agent traffic.",
    "Lines 5 + 6 fail closed:  any outbound the agent didn't get through loopback or DNS gets dropped.",
    "Lines 6 + 7 are why the agent can still talk to the world:  ports 80 and 443 are NAT-redirected to the router's transparent proxy on :8444, where every call is policy-checked and audited.",
  ], { x: 8.1, y: 2.6 });
  s.addText("controller/src/reconciler/mod.rs:1916-1958", {
    x: M, y: 6.8, w: W - 2 * M, h: 0.3,
    fontFace: F_CODE, fontSize: 10, color: QUIET, margin: 0,
  });
}

// slide 7: MESH — Victor style, one named artefact (the KNOCK frame itself)
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3.2 · mesh");
  title(s, "Agents authenticate each other.", { fontSize: 42 });
  lede(s,
    "Every inter-agent message rides Signal Protocol:  Ed25519 + X25519 identities, X3DH for " +
    "session establishment, Double Ratchet for forward-secret message keys. The relay routes " +
    "opaque bytes and never sees plaintext.",
    { y: 2.5, h: 1.4 }
  );

  // Show a real KNOCK frame as the visual artefact
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

  // right column: per-field meaning
  s.addText("The KNOCK frame", {
    x: 8.1, y: 3.85, w: W - 8.1 - M, h: 0.4,
    fontFace: F_DISPLAY, fontSize: 18, bold: true, color: INK, margin: 0,
  });
  s.addText(
    [
      { text: "Carries the initiator's X3DH establishment.  Receiver's policy hook gates accept BEFORE the session opens.", options: { breakLine: true } },
      { text: " " , options: { breakLine: true, fontSize: 6 } },
      { text: "Once accepted, all subsequent frames use Double Ratchet keys.  Compromising one frame's key does not compromise past frames.", options: {} },
    ],
    {
      x: 8.1, y: 4.35, w: W - 8.1 - M, h: 3.0,
      fontFace: F_BODY, fontSize: 13, color: INK, margin: 0,
      paraSpaceAfter: 6,
    });
}

// slide 8: GOVERNANCE — Stripe-docs style, real InferencePolicy CR snippet + prose
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3.3 · governance");
  title(s, "Policy is data.", { fontSize: 42 });
  codeBlock(s, [
    { t: "apiVersion: ", k: "kw" }, { t: "kars.azure.com/v1alpha1\n", k: "str" },
    { t: "kind: ", k: "kw" }, { t: "InferencePolicy\n", k: "str" },
    { t: "metadata:\n  name: research-agent-policy\n" },
    { t: "spec:\n" },
    { t: "  modelPreference:\n" },
    { t: "    primary: { provider: ", k: "kw" }, { t: "github-copilot", k: "str" }, { t: ", deployment: ", k: "kw" }, { t: "claude-opus-4.7 ", k: "str" }, { t: "}\n" },
    { t: "    fallback:\n" },
    { t: "      - { provider: ", k: "kw" }, { t: "github-copilot", k: "str" }, { t: ", deployment: ", k: "kw" }, { t: "gpt-5 ", k: "str" }, { t: "}\n" },
    { t: "      - { provider: ", k: "kw" }, { t: "github-copilot", k: "str" }, { t: ", deployment: ", k: "kw" }, { t: "claude-sonnet-4.5 ", k: "str" }, { t: "}\n" },
    { t: "  tokenBudget:\n" },
    { t: "    dailyTokens: ", k: "kw" }, { t: "1000000\n", k: "str" },
    { t: "    perRequestTokens: ", k: "kw" }, { t: "32000\n", k: "str" },
    { t: "  contentSafety:\n" },
    { t: "    requirePromptShields: ", k: "kw" }, { t: "true\n", k: "str" },
  ], { x: M, y: 2.5, w: 7.0, h: 4.2, fontSize: 12 });

  rightProse(s, [
    "Every model call goes through a per-sandbox sidecar that loads this CR and enforces it byte-for-byte.",
    "Primary returns 503?  Router walks the fallback chain.  Token budget exhausted?  Router refuses before any upstream call.",
    "Prompt Shields hard-required?  Router fails closed if the upstream guardrail annotation is missing.",
    "Every decision lands in a hash-chained audit JSONL.  prev_hash + hash per row — tamper-evident.",
  ], { x: 8.1, y: 2.5 });
  s.addText("inference-router/src/{failover,budget,safety,routes/chat_completions}.rs", {
    x: M, y: 6.9, w: W - 2 * M, h: 0.3,
    fontFace: F_CODE, fontSize: 10, color: QUIET, margin: 0,
  });
}

// slide 9: GOVERNANCE — the four layers (annotated stack, real labels)
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§3.3 · governance · defense in depth");
  title(s, "Bypass one.  Three more deny.", { fontSize: 38 });
  // Vertical stack of named layers, with the protected operation on the right
  const layers = [
    ["iptables egress-guard", "Init container · UID 1000 → loopback + DNS only · drops everything else", "controller/src/reconciler/mod.rs:1916"],
    ["NetworkPolicy", "Per-sandbox · default deny · explicit allow for relay :8443 + registry :8080 + auth-sidecar :5000", "controller/src/reconciler/mod.rs:870-1042"],
    ["Inference router", "InferencePolicy · ToolPolicy · Content Safety · per-sandbox token budget", "inference-router/src/routes/chat_completions.rs"],
    ["AGT policy hook", "Per-tool-call evaluator · trust-score gate on sub-agent spawn", "runtimes/openclaw/src/index.ts:683"],
  ];
  const lh = 0.95, lgap = 0.15;
  const y0 = 2.6;
  layers.forEach(([name, body, ref], i) => {
    const y = y0 + i * (lh + lgap);
    s.addShape(pres.shapes.RECTANGLE, {
      x: M, y, w: 0.08, h: lh,
      fill: { color: ACCENT }, line: { color: ACCENT, width: 0 },
    });
    s.addText(`${i + 1}`, {
      x: M + 0.2, y, w: 0.5, h: lh,
      fontFace: F_DISPLAY, fontSize: 26, bold: true, color: ACCENT,
      valign: "middle", margin: 0,
    });
    s.addText(name, {
      x: M + 0.8, y, w: 4.0, h: 0.4,
      fontFace: F_DISPLAY, fontSize: 18, bold: true, color: INK,
      valign: "top", margin: 0,
    });
    s.addText(body, {
      x: M + 0.8, y: y + 0.4, w: 7.6, h: 0.5,
      fontFace: F_BODY, fontSize: 12, color: MUTED,
      valign: "top", margin: 0,
    });
    s.addText(ref, {
      x: M + 8.5, y, w: 3.3, h: lh,
      fontFace: F_CODE, fontSize: 10, color: QUIET,
      align: "right", valign: "middle", margin: 0,
    });
  });
}

// slide 10: BLUEPRINTS — six shapes, real labels per shape (matrix, not over-simplified)
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§4 · deployment");
  title(s, "Six shapes.  One chart.", { fontSize: 38 });
  lede(s,
    "The KarsSandbox CRD + the helm chart are identical across all six. What differs is the " +
    "trust boundary, the model provider, and whether the relay is local or federated.",
    { y: 2.5, h: 1.0 }
  );
  // 6 tiles in 3×2 grid; each tile has number, title, sub-line, three meta tags
  const bps = [
    ["01", "Developer inner loop", "laptop · docker", "single image · no cluster · 30s start"],
    ["02", "Local Kubernetes dev", "laptop · kind", "real K8s · helm + Headlamp"],
    ["03", "Enterprise self-hosted", "AKS · single tenant", "private VNet · WI · audit retention"],
    ["04", "Managed public offload", "AKS · multi-tenant", "Kata + SEV-SNP · provider untrusted"],
    ["05", "Cross-org federation", "two AKS clusters", "A2A bridge · double policy evaluation"],
    ["06", "Sovereign / air-gapped", "isolated AKS", "private model · signed allowlist · no egress"],
  ];
  const tw = 3.85, th = 1.6, gap = 0.2;
  const cols = 3;
  const totalW = cols * tw + (cols - 1) * gap;
  const x0 = (W - totalW) / 2;
  const y0 = 3.9;
  bps.forEach(([num, name, where, meta], i) => {
    const col = i % cols, row = Math.floor(i / cols);
    const x = x0 + col * (tw + gap);
    const y = y0 + row * (th + gap);
    s.addShape(pres.shapes.RECTANGLE, {
      x, y, w: tw, h: th,
      fill: { color: PAPER }, line: { color: ACCENT, width: 1 },
    });
    s.addText(num, {
      x: x + 0.2, y: y + 0.15, w: 0.6, h: 0.35,
      fontFace: F_CODE, fontSize: 12, color: ACCENT, margin: 0,
    });
    s.addText(name, {
      x: x + 0.8, y: y + 0.15, w: tw - 1.0, h: 0.4,
      fontFace: F_DISPLAY, fontSize: 15, bold: true, color: INK, margin: 0,
    });
    s.addText(where, {
      x: x + 0.8, y: y + 0.5, w: tw - 1.0, h: 0.3,
      fontFace: F_BODY, fontSize: 12, color: MUTED, margin: 0,
    });
    s.addText(meta, {
      x: x + 0.2, y: y + 1.0, w: tw - 0.4, h: 0.5,
      fontFace: F_BODY, fontSize: 11, italic: true, color: ACCENT, margin: 0,
    });
  });
}

// slide 11: MULTI-RUNTIME — 8 wired runtimes with what each is + LOC honest
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§5 · runtimes");
  title(s, "Eight wired.  Bring your own for the ninth.", { fontSize: 36 });
  lede(s,
    "WIRED_KINDS in cli/src/runtime.ts is the single source of truth.  Adding a new runtime: " +
    "new image, new entrypoint, new controller branch, new CRD variant — call it a few days, not a few weeks.",
    { y: 2.5, h: 1.1 }
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

// slide 12: BUILT ON AGT (Stripe-press style: clean statement + named PRs flowing back)
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§6 · upstream");
  title(s, "Built on the Microsoft Agent Governance Toolkit.", { fontSize: 32 });
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

// slide 13: WHAT'S NEXT (four real targets with one-line scope each)
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§7 · next");
  title(s, "Four shipping targets.", { fontSize: 38 });
  primitiveRow(s, [
    ["Hermes Act 2", "Python mesh client at TypeScript SDK parity"],
    ["kars-sre", "In-cluster SRE agent · auto-diagnose · approval-gated fixes"],
    ["Sovereign GA", "Blueprint 06 from compose-by-hand to one-command bundle"],
    ["Attestation", "Kata + SEV-SNP attestation-gated workloads"],
  ], { y: 2.7 });
  // long meta paragraph below, smaller, gives texture
  lede(s,
    "Each of these is a separate PR series with its own design doc under docs/blueprints/ and " +
    "docs/proposals/.  The kars-sre design (07-kars-sre-proposal.md) is the most concrete — " +
    "designed against a regression corpus of 16 OOTB blockers an in-cluster agent would have caught.",
    { y: 5.4, h: 1.5, fontSize: 14, color: MUTED }
  );
}

// slide 14: TRY IT (Stripe-docs style: monospace command block, real)
{
  const s = light();
  page++;
  pageNum(s, page);
  eyebrow(s, "§8 · try it");
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
  ], { x: M, y: 4.2, w: W - 2 * M, h: 2.6, fontSize: 14 });
}

// slide 15: CLOSE (dark, big mark, three-word tagline)
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
