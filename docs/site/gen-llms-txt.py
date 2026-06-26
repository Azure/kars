#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Generates docs/llms.txt — a machine-readable index of the documentation for
# LLM/agent tooling, following the llms.txt convention (https://llmstxt.org/).
# Built from docs/SUMMARY.md (the canonical nav) + the first prose line of each
# page. Re-run after changing SUMMARY.md or page intros:
#
#   python3 docs/site/gen-llms-txt.py
#
import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DOCS = os.path.join(ROOT, "docs")
SUMMARY = os.path.join(DOCS, "SUMMARY.md")
OUT = os.path.join(DOCS, "llms.txt")
REPO = "https://github.com/Azure/kars"
BLOB = f"{REPO}/blob/main/docs"

LINK = re.compile(r"\[([^\]]+)\]\(([^)]+)\)")


def first_paragraph(md_path):
    """Return the first non-heading, non-blockquote prose line of a page."""
    if not os.path.exists(md_path):
        return ""
    in_fence = False
    for line in open(md_path, encoding="utf-8", errors="replace"):
        s = line.strip()
        if s.startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence or not s:
            continue
        # skip HTML comments (license headers etc.) and HTML blocks
        if s.startswith("<!--") or s.startswith("<"):
            continue
        if s.startswith(("#", ">", "|", "-", "*", "!")):
            continue
        # skip license/boilerplate first lines
        low = s.lower()
        if low.startswith(("copyright", "licensed under", "spdx")):
            continue
        # skip badge / image rows (e.g. "[![npm](…)](…)" or HTML-only lines)
        if s.startswith("[!") or re.match(r"^\[?!\[", s):
            continue
        # strip inline markdown for a clean one-liner
        s = re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", s)
        s = re.sub(r"[`*_]", "", s)
        if not s.strip():
            continue
        # truncate on a word boundary near 200 chars
        if len(s) > 200:
            s = s[:200].rsplit(" ", 1)[0] + "…"
        return s
    return ""


def main():
    out = []
    out.append("# kars")
    out.append("")
    out.append(
        "> kars is a secure, Kubernetes-native runtime for AI agents on Azure: "
        "one hardened sandbox per agent, zero credentials in the agent process, "
        "an in-pod inference router that brokers every external call, and an "
        "end-to-end encrypted inter-agent mesh. Governance is consumed from the "
        "Microsoft Agent Governance Toolkit (AGT)."
    )
    out.append("")
    out.append(f"Source: {REPO} (MIT). Docs are mdBook Markdown under `docs/`.")
    out.append("")

    section = None
    for raw in open(SUMMARY, encoding="utf-8"):
        line = raw.rstrip("\n")
        h = re.match(r"^#\s+(.*)", line)
        if h:
            section = h.group(1).strip()
            if section.lower() != "summary":
                out.append("")
                out.append(f"## {section}")
            continue
        m = LINK.search(line)
        if not m:
            continue
        title, href = m.group(1), m.group(2)
        if href.startswith("http"):
            continue
        md_path = os.path.normpath(os.path.join(DOCS, href.split("#")[0]))
        summary = first_paragraph(md_path)
        url = f"{BLOB}/{href}"
        indent = "  " if raw.startswith(("  ", "\t")) else ""
        if summary:
            out.append(f"{indent}- [{title}]({url}): {summary}")
        else:
            out.append(f"{indent}- [{title}]({url})")

    text = "\n".join(out).rstrip() + "\n"
    open(OUT, "w", encoding="utf-8").write(text)
    print(f"Wrote {OUT} ({len(text)} bytes, {text.count(chr(10))} lines)")


if __name__ == "__main__":
    main()
