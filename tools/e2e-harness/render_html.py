#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
render_html.py — convert the harness's final brief (transcript.log)
into a polished, browser-openable HTML file.

Rewrites sandbox-local image refs (`/sandbox/.openclaw/workspace/incoming/X.png`)
to the relative filenames that `kubectl cp` lands next to the HTML, so
the operator can open out/<runId>/brief.html and see the same brief
the writer agent produced — hero image, scorecard image, footnotes,
links, headings — without the agent's container being involved.

No external dependencies — uses stdlib only so it works on any
operator's machine. The HTML uses an inline GitHub-flavoured CSS
sheet so the page looks like a polished doc not a raw markdown dump.
"""
from __future__ import annotations

import html
import os
import re
import sys
from pathlib import Path

HTML_HEAD = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
  :root {{
    --bg: #ffffff; --fg: #1f2328; --muted: #59636e;
    --accent: #0969da; --border: #d1d9e0; --code-bg: #f6f8fa;
  }}
  @media (prefers-color-scheme: dark) {{
    :root {{ --bg:#0d1117; --fg:#e6edf3; --muted:#7d8590; --accent:#2f81f7; --border:#30363d; --code-bg:#161b22; }}
  }}
  html, body {{ background: var(--bg); color: var(--fg);
    font-family: -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
    line-height: 1.55; margin: 0; padding: 0; }}
  main {{ max-width: 820px; margin: 0 auto; padding: 48px 24px 96px; }}
  header.banner {{ border-bottom: 1px solid var(--border); padding-bottom: 16px; margin-bottom: 32px; }}
  header.banner h1 {{ margin: 0 0 6px; font-size: 22px; }}
  header.banner .meta {{ color: var(--muted); font-size: 14px; }}
  h1, h2, h3 {{ line-height: 1.25; margin-top: 32px; margin-bottom: 12px; }}
  h1 {{ font-size: 28px; border-bottom: 1px solid var(--border); padding-bottom: 8px; }}
  h2 {{ font-size: 22px; }}
  h3 {{ font-size: 18px; }}
  p {{ margin: 0 0 12px; }}
  a {{ color: var(--accent); text-decoration: none; }}
  a:hover {{ text-decoration: underline; }}
  img {{ max-width: 100%; height: auto; border: 1px solid var(--border); border-radius: 8px; margin: 16px 0; display: block; }}
  ol, ul {{ padding-left: 28px; }}
  ol li, ul li {{ margin-bottom: 6px; }}
  code {{ background: var(--code-bg); padding: 1px 6px; border-radius: 4px;
    font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 88%; }}
  pre {{ background: var(--code-bg); padding: 14px; border-radius: 8px; overflow-x: auto; }}
  pre code {{ background: transparent; padding: 0; }}
  blockquote {{ border-left: 4px solid var(--border); padding: 4px 16px;
    color: var(--muted); margin: 16px 0; }}
  hr {{ border: 0; border-top: 1px solid var(--border); margin: 32px 0; }}
  .footnotes {{ font-size: 14px; color: var(--muted); margin-top: 48px;
    border-top: 1px solid var(--border); padding-top: 16px; }}
  .badges {{ display: flex; gap: 8px; flex-wrap: wrap; margin: 12px 0 0; }}
  .badge {{ display: inline-flex; align-items: center; gap: 6px;
    background: var(--code-bg); border: 1px solid var(--border);
    border-radius: 999px; padding: 2px 10px; font-size: 12px; color: var(--muted); }}
  .badge.ok {{ color: #1a7f37; border-color: #1a7f37; }}
</style>
</head>
<body>
<main>
<header class="banner">
  <h1>{title}</h1>
  <div class="meta">{meta}</div>
  <div class="badges">{badges}</div>
</header>
"""

HTML_FOOT = """
</main>
</body>
</html>
"""

# ─── Minimal markdown → HTML ────────────────────────────────────────────
# Stdlib-only renderer scoped to what the brief actually emits:
# headings, paragraphs, ordered+unordered lists, inline links/images,
# code spans, and `[N]` footnote refs. Anything more exotic the writer
# agent doesn't produce, so we keep this tight rather than pull in
# `markdown` or `mistune` (no external deps = works on every operator's
# machine).

INLINE_LINK_RE = re.compile(r"\[([^\]]+)\]\(([^)]+)\)")
INLINE_IMG_RE = re.compile(r"!\[([^\]]*)\]\(([^)]+)\)")
INLINE_CODE_RE = re.compile(r"`([^`]+)`")
FOOTNOTE_REF_RE = re.compile(r"\[(\d+)\]")
NUMBERED_FOOTNOTE_RE = re.compile(r"^(\d+)\.\s+(https?://\S+)\s*$")


def rewrite_images(md: str, image_map: dict[str, str]) -> str:
    """Rewrite `![alt](/sandbox/.../X.png)` → `![alt](X.png)` for any
    image the caller copied into OUT_DIR. Images not on disk are kept
    as-is and will show as broken-image icons in the browser."""
    def sub(m: re.Match) -> str:
        alt, src = m.group(1), m.group(2)
        basename = os.path.basename(src)
        if basename in image_map:
            return f"![{alt}]({image_map[basename]})"
        return m.group(0)
    return INLINE_IMG_RE.sub(sub, md)


def render_inline(text: str) -> str:
    text = html.escape(text, quote=False)
    text = INLINE_IMG_RE.sub(
        lambda m: f'<img alt="{html.escape(m.group(1), quote=True)}" src="{html.escape(m.group(2), quote=True)}">',
        text,
    )
    text = INLINE_LINK_RE.sub(
        lambda m: f'<a href="{html.escape(m.group(2), quote=True)}" target="_blank" rel="noopener">{m.group(1)}</a>',
        text,
    )
    text = INLINE_CODE_RE.sub(lambda m: f"<code>{m.group(1)}</code>", text)
    text = FOOTNOTE_REF_RE.sub(
        lambda m: f'<sup><a href="#fn-{m.group(1)}">[{m.group(1)}]</a></sup>',
        text,
    )
    return text


def md_to_html(md: str) -> str:
    """Tiny markdown subset. Walk lines; emit blocks as we group them."""
    out: list[str] = []
    lines = md.splitlines()
    i = 0
    in_footnotes = False

    while i < len(lines):
        line = lines[i]
        stripped = line.strip()

        # Blank line → close any open block
        if not stripped:
            i += 1
            continue

        # Standalone image on its own line — emit as <img> not <p><img></p>
        if INLINE_IMG_RE.fullmatch(stripped):
            m = INLINE_IMG_RE.fullmatch(stripped)
            assert m is not None
            out.append(
                f'<img alt="{html.escape(m.group(1), quote=True)}" '
                f'src="{html.escape(m.group(2), quote=True)}">'
            )
            i += 1
            continue

        # ATX headings (#, ##, ###)
        h = re.match(r"^(#{1,6})\s+(.*)$", stripped)
        if h:
            level = len(h.group(1))
            out.append(f"<h{level}>{render_inline(h.group(2))}</h{level}>")
            i += 1
            continue

        # Setext-like "1. Trends" patterns the brief uses for section
        # heads (number-period-text, no markdown #). Promote to <h2>.
        sect = re.match(r"^(\d+)\.\s+([A-Z][A-Za-z][^.?!]*?)\s*$", stripped)
        if sect and not NUMBERED_FOOTNOTE_RE.match(stripped):
            # Heuristic: section heads are short (< 70 chars) and have
            # no URL — distinguishes "1. Trends" from numbered footnotes.
            if "http" not in stripped and len(stripped) < 80:
                out.append(f'<h2>{html.escape(sect.group(1))}. {render_inline(sect.group(2))}</h2>')
                i += 1
                continue

        # Numbered footnote line ("1. https://…")
        fn = NUMBERED_FOOTNOTE_RE.match(stripped)
        if fn:
            if not in_footnotes:
                out.append('<section class="footnotes"><h2>Footnotes</h2><ol>')
                in_footnotes = True
            n, url = fn.group(1), fn.group(2)
            esc = html.escape(url, quote=True)
            out.append(
                f'<li id="fn-{n}"><a href="{esc}" target="_blank" rel="noopener">{esc}</a></li>'
            )
            i += 1
            continue

        # Footnote-like "1. DeepWiki…: https://…" (number + label + URL)
        fn2 = re.match(r"^(\d+)\.\s+(.+?:\s*https?://\S+)\s*$", stripped)
        if fn2:
            if not in_footnotes:
                out.append('<section class="footnotes"><h2>Footnotes</h2><ol>')
                in_footnotes = True
            n, body = fn2.group(1), fn2.group(2)
            out.append(f'<li id="fn-{n}">{render_inline(body)}</li>')
            i += 1
            continue

        # Bullet list
        if stripped.startswith(("- ", "* ")):
            out.append("<ul>")
            while i < len(lines) and lines[i].strip().startswith(("- ", "* ")):
                item = lines[i].strip()[2:]
                out.append(f"<li>{render_inline(item)}</li>")
                i += 1
            out.append("</ul>")
            continue

        # Plain paragraph — gather contiguous non-empty non-heading lines
        para: list[str] = []
        while i < len(lines):
            l = lines[i]
            ls = l.strip()
            if not ls:
                break
            if re.match(r"^#{1,6}\s", ls):
                break
            if INLINE_IMG_RE.fullmatch(ls):
                break
            if NUMBERED_FOOTNOTE_RE.match(ls) or re.match(r"^\d+\.\s+.+?:\s*https?://", ls):
                break
            para.append(ls)
            i += 1
        if para:
            out.append(f"<p>{render_inline(' '.join(para))}</p>")

    if in_footnotes:
        out.append("</ol></section>")
    return "\n".join(out)


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: render_html.py <out_dir>", file=sys.stderr)
        return 1
    out_dir = Path(sys.argv[1]).resolve()
    transcript = out_dir / "transcript.log"
    if not transcript.exists():
        print(f"transcript.log not found in {out_dir}", file=sys.stderr)
        return 1

    md = transcript.read_text(encoding="utf-8", errors="replace")

    # Build the image map by looking for any *.png/jpg the artifact
    # collector dropped next to transcript.log.
    image_map: dict[str, str] = {}
    for ext in ("png", "jpg", "jpeg", "gif", "svg"):
        for p in out_dir.glob(f"*.{ext}"):
            image_map[p.name] = p.name

    md_rewritten = rewrite_images(md, image_map)

    # Pull a few facts for the header banner.
    verify_json = out_dir / "verify.json"
    badges: list[str] = []
    meta_parts: list[str] = []
    if verify_json.exists():
        import json
        try:
            data = json.loads(verify_json.read_text(encoding="utf-8"))
            scenario = data.get("scenario") or "—"
            checks = data.get("checks", [])
            passed = sum(1 for c in checks if c.get("passed"))
            verdict = "PASS" if data.get("all_passed") else "FAIL"
            badge_cls = "ok" if verdict == "PASS" else ""
            badges.append(f'<span class="badge {badge_cls}">verify: {verdict} ({passed}/{len(checks)})</span>')
            meta_parts.append(f"scenario: <code>{html.escape(scenario)}</code>")
        except Exception:
            pass
    meta_parts.append(f"run: <code>{html.escape(out_dir.name)}</code>")
    word_count = len(md.split())
    badges.append(f'<span class="badge">{word_count} words</span>')
    badges.append(f'<span class="badge">{len(image_map)} embedded images</span>')

    # Pull token usage from response.json if present.
    response_json = out_dir / "response.json"
    if response_json.exists():
        try:
            import json
            resp = json.loads(response_json.read_text(encoding="utf-8"))
            usage = resp.get("usage", {})
            inp = usage.get("prompt_tokens", "?")
            out_t = usage.get("completion_tokens", "?")
            badges.append(f'<span class="badge">{inp} in / {out_t} out tokens</span>')
        except Exception:
            pass

    title = "kars e2e harness — final brief"
    body_html = md_to_html(md_rewritten)

    html_out = HTML_HEAD.format(
        title=html.escape(title),
        meta=" · ".join(meta_parts),
        badges="".join(badges),
    ) + body_html + HTML_FOOT

    target = out_dir / "brief.html"
    target.write_text(html_out, encoding="utf-8")
    print(f"wrote {target}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
