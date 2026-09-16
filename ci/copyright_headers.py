#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Check/apply repository licensing coverage without rewriting file bodies."""

import argparse
import codecs
from collections import Counter
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys
import tokenize


COPYRIGHT = "Copyright (c) Microsoft Corporation."
LICENSE = "Licensed under the MIT License."
POLICY_PATH = "ci/copyright-coverage.json"
STYLES = {
    "slash": f"// {COPYRIGHT}\n// {LICENSE}\n\n",
    "hash": f"# {COPYRIGHT}\n# {LICENSE}\n\n",
    "html": f"<!-- {COPYRIGHT}\n{LICENSE} -->\n\n",
    "css": f"/* {COPYRIGHT}\n{LICENSE} */\n\n",
    # No whitespace outside either template comment: rendering stays identical,
    # including when the original template begins with a whitespace-trimming tag.
    "helm": f"{{{{/* {COPYRIGHT}\n{LICENSE} */}}}}",
    "handlebars": f"{{{{!-- {COPYRIGHT}\n{LICENSE} --}}}}",
}
SUFFIX_STYLES = {
    **dict.fromkeys((".rs", ".go", ".ts", ".tsx", ".js", ".mjs", ".bicep"), "slash"),
    **dict.fromkeys((".sh", ".py", ".toml", ".yaml", ".yml"), "hash"),
    ".md": "html",
    ".css": "css",
    ".hbs": "handlebars",
    ".tpl": "helm",
}
HASH_NAMES = {
    ".gitignore", ".dockerignore", "Makefile", "CODEOWNERS", ".env.example",
    "requirements.txt",
}
DOCKER_DIRECTIVE = re.compile(rb"^[ \t]*#[ \t]*(syntax|escape|check)[ \t]*=", re.I)
ENCODING_COOKIE = re.compile(rb"^[ \t\f]*#.*?coding[:=][ \t]*([-_.a-zA-Z0-9]+)")


class CoverageError(ValueError):
    """A file cannot be classified or safely changed."""


def load_policy(root):
    policy = json.loads((root / POLICY_PATH).read_bytes())
    if not isinstance(policy, dict) or set(policy) != {"version", "formats", "files"} or policy["version"] != 1:
        raise CoverageError("unsupported coverage policy schema")
    categories = {"repository-license", "third-party", "legal"}
    for group in ("formats", "files"):
        allowed_categories = categories | ({"generated"} if group == "files" else set())
        if not isinstance(policy[group], dict):
            raise CoverageError(f"{group} must be an object")
        for key, rule in policy[group].items():
            if (
                not isinstance(rule, dict)
                or set(rule) != {"category", "reason", "notice"}
                or not isinstance(rule["category"], str)
                or rule["category"] not in allowed_categories
                or not isinstance(rule["reason"], str)
                or not rule["reason"].strip()
                or not isinstance(rule["notice"], str)
                or rule["notice"] not in ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.txt")
            ):
                raise CoverageError(f"invalid coverage rule: {key}")
            if group == "files":
                path = PurePosixPath(key)
                if path.is_absolute() or ".." in path.parts or str(path) != key:
                    raise CoverageError(f"invalid coverage path: {key}")
            elif not key.startswith(".") or "/" in key:
                raise CoverageError(f"invalid coverage extension: {key}")
    for notice in ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.txt"):
        if not (root / notice).is_file():
            raise CoverageError(f"missing license/notice document: {notice}")
    return policy


def classification(path, policy, data=b""):
    p = PurePosixPath(path)
    if path in policy["files"]:
        return dict(policy["files"][path])
    if p.parts[0] == "vendor":
        return {
            "category": "third-party", "notice": "NOTICE",
            "reason": "Vendored inputs retain their upstream/package licenses and checksums.",
        }
    if "templates" in p.parts and p.suffix in (".yaml", ".yml"):
        return {"category": "header", "style": template_yaml_style(data)}
    if "templates" in p.parts and p.suffix in (".tpl", ".txt"):
        return {"category": "header", "style": "helm"}
    if p.name.startswith("Dockerfile") and (p.name == "Dockerfile" or p.name[10:11] == "."):
        return {"category": "header", "style": "hash"}
    if p.name in HASH_NAMES or path == "tools/drift/allowlist-q1.txt":
        return {"category": "header", "style": "hash"}
    if p.suffix in SUFFIX_STYLES:
        return {"category": "header", "style": SUFFIX_STYLES[p.suffix]}
    if p.suffix in policy["formats"]:
        return dict(policy["formats"][p.suffix])
    raise CoverageError("unknown format: add a safe comment style or an explicit reviewed coverage rule")


def anchor(path, data):
    """Return an insertion point after syntax that must remain at the start."""
    p = PurePosixPath(path)
    start = len(codecs.BOM_UTF8) if data.startswith(codecs.BOM_UTF8) else 0
    body = data[start:]
    lines = body.splitlines(keepends=True)
    shebang = lines and lines[0].startswith(b"#!")
    if p.suffix == ".rs" and body.startswith(b"#!["):
        shebang = False
    count = 1 if shebang else 0
    if p.suffix == ".py":
        try:
            encoding, detected_lines = tokenize.detect_encoding(io.BytesIO(data).readline)
            data.decode(encoding)
        except (SyntaxError, UnicodeError, LookupError) as exc:
            raise CoverageError(f"invalid Python encoding: {exc}") from exc
        for i, line in enumerate(lines[:len(detected_lines)]):
            if ENCODING_COOKIE.match(line):
                count = max(count, i + 1)
    else:
        try:
            data.decode("utf-8-sig")
        except UnicodeError as exc:
            raise CoverageError("commentable files must be UTF-8 (Python cookies are supported)") from exc
    if b"\0" in data:
        raise CoverageError("binary content in a commentable format")
    if p.name == "Dockerfile" or p.name.startswith("Dockerfile."):
        count = 0
        for line in lines:
            if not DOCKER_DIRECTIVE.match(line):
                break
            count += 1
    if p.suffix == ".md" and lines and lines[0].strip() in (b"---", b"+++"):
        delimiter = lines[0].strip()
        endings = (delimiter, b"...") if delimiter == b"---" else (delimiter,)
        for i, line in enumerate(lines[1:], 1):
            if line.strip() in endings:
                count = i + 1
                break
        else:
            raise CoverageError("unterminated Markdown frontmatter")
    if p.suffix == ".css" and body.startswith(b'@charset "'):
        match = re.match(rb'@charset "[^"\r\n]+";', body)
        if not match:
            raise CoverageError("invalid CSS charset directive")
        # A CSS comment may immediately follow the semicolon, even on one line.
        return start + match.end()
    if count and not lines[count - 1].endswith(b"\n"):
        raise CoverageError("leading directive has no newline; terminate it before applying a header")
    return start + sum(map(len, lines[:count]))


def header_for(style, data):
    first_newline = data.find(b"\n")
    newline = "\r\n" if first_newline > 0 and data[first_newline - 1:first_newline] == b"\r" else "\n"
    return STYLES[style].replace("\n", newline).encode("ascii")


def yaml_document_start(data, start):
    cursor = start
    for line in data[start:].splitlines(keepends=True):
        token = line.strip()
        if re.fullmatch(rb"---(?:[ \t]+#.*)?", token):
            if not line.endswith(b"\n"):
                raise CoverageError("leading YAML document marker needs a terminating newline")
            return cursor + len(line)
        if token.startswith((b"--- ", b"---\t")):
            raise CoverageError("inline YAML document content needs reviewed header placement")
        if token and not token.startswith((b"#", b"%")):
            break
        cursor += len(line)
    return start


def yaml_license_prefix(data):
    start = len(codecs.BOM_UTF8) if data.startswith(codecs.BOM_UTF8) else 0
    for offset in (start, yaml_document_start(data, start)):
        for style in ("hash", "helm"):
            prefix = header_for(style, data[offset:])
            if data[offset:].startswith(prefix):
                return offset, prefix
    return start, b""


def template_yaml_style(data):
    offset, prefix = yaml_license_prefix(data)
    body = data[:offset] + data[offset + len(prefix):]
    start = len(codecs.BOM_UTF8) if body.startswith(codecs.BOM_UTF8) else 0
    for line in body[yaml_document_start(body, start):].splitlines():
        token = line.strip()
        if token and not token.startswith(b"#"):
            # Only a leading Helm action can chomp a preceding license comment
            # into the first YAML token. Plain/dual-use YAML must remain raw-parseable.
            return "helm" if token.startswith(b"{{") else "hash"
    return "hash"


def has_header(data, offset, style):
    prefix = data[offset:].replace(b"\r\n", b"\n")
    expected = STYLES[style].encode("ascii").rstrip(b"\n")
    prefix = prefix.lstrip(b"\n")
    if style in ("hash", "slash"):
        marker = b"#" if style == "hash" else b"//"
        lines = prefix.splitlines()[:5]
        # Legacy LOC annotations can separate the notices. Both must be exact
        # comment lines in the leading preamble, never executable/string text.
        if not lines or lines[0] != marker + b" " + COPYRIGHT.encode("ascii"):
            return False
        for line in lines[1:]:
            if line == marker + b" " + LICENSE.encode("ascii"):
                return True
            if line.strip() and not line.startswith(marker):
                return False
        return False
    if style == "html":
        alternative = f"<!--\n{COPYRIGHT}\n{LICENSE}\n-->".encode("ascii")
        if prefix.startswith(alternative):
            return True
    return prefix.startswith(expected) and (
        style in ("helm", "handlebars", "html", "css")
        or len(prefix) == len(expected)
        or prefix[len(expected):len(expected) + 1] == b"\n"
    )


def insertion(path, data, style):
    offset = anchor(path, data)
    p = PurePosixPath(path)
    if style == "hash" and "templates" in p.parts and p.suffix in (".yaml", ".yml"):
        # A license-only chunk before "---" becomes an extra Helm document.
        offset = yaml_document_start(data, offset)
    if has_header(data, offset, style):
        return offset, b""
    if PurePosixPath(path).suffix == ".rs":
        # Older fuzz targets put their existing notice after #![no_main].
        # Preserve it without treating new Rust attributes as executable shebangs.
        attribute = re.match(rb"#!\[[^\r\n]*\]\r?\n", data[offset:])
        if attribute and has_header(data, offset + attribute.end(), style):
            return offset + attribute.end(), b""
    return offset, header_for(style, data)


def header_edit(path, data, style):
    offset, header = insertion(path, data, style)
    if header and PurePosixPath(path).suffix in (".yaml", ".yml"):
        start, previous = yaml_license_prefix(data)
        if previous:
            body = data[:start] + data[start + len(previous):]
            destination, header = insertion(path, body, style)
            # Relocate only our license block; preamble comments, delimiters and
            # every other body byte retain their original order and content.
            return start, len(previous), destination, header
    return offset, 0, offset, header


def tracked_files(root):
    result = subprocess.check_output(["git", "ls-files", "-z"], cwd=root)
    return sorted(set(result.decode("utf-8").split("\0")) - {""})


def file_bytes(root, name):
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or str(path) != name:
        raise CoverageError("path must be repository-relative and normalized")
    target = root
    for part in path.parts:
        target /= part
        if target.is_symlink():
            raise CoverageError("symlink requires explicit human review; never follow it")
    mode = target.stat().st_mode
    if not stat.S_ISREG(mode):
        raise CoverageError("not a regular file")
    return target.read_bytes(), mode


def process(root, paths, policy, apply=False):
    records, changes = [], []
    for name in sorted(set(paths)):
        record = {"path": name}
        try:
            data, mode = file_bytes(root, name)
            record.update(classification(name, policy, data))
            if record["category"] == "header":
                remove_offset, removed, offset, header = header_edit(name, data, record["style"])
                record["status"] = "missing" if header or removed else "present"
                if header or removed:
                    body = data[:remove_offset] + data[remove_offset + removed:]
                    updated = body[:offset] + header + body[offset:]
                    record.update({
                        "offset": offset, "inserted_bytes": len(header),
                        "removed_offset": remove_offset,
                        "removed_bytes": removed,
                        "before_sha256": hashlib.sha256(data).hexdigest(),
                        "after_sha256": hashlib.sha256(updated).hexdigest(),
                    })
                    if removed:
                        record["diagnostic"] = "existing Microsoft + MIT header has incorrect syntax or placement"
                    changes.append((name, data, mode, updated, record))
            else:
                record["status"] = "covered-without-header"
        except (CoverageError, OSError, UnicodeError) as exc:
            record.update(status="error", error=str(exc))
        records.append(record)
    # Fail closed, before writing any file, if coverage is incomplete/unsafe.
    if apply and not any(r["status"] == "error" for r in records):
        for name, data, mode, updated, record in changes:
            current, current_mode = file_bytes(root, name)
            if current != data or current_mode != mode:
                raise CoverageError(f"{name}: changed during inspection; nothing should overwrite another editor")
        for name, data, mode, updated, record in changes:
            target = root / name
            target.write_bytes(updated)
            if target.stat().st_mode != mode:
                raise CoverageError(f"{name}: file mode changed")
            record["status"] = "applied"
    return records


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "apply"))
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--report", type=Path, help="write exhaustive per-file JSON coverage (relative to root)")
    parser.add_argument("--verbose", action="store_true", help="list every non-header coverage exception")
    parser.add_argument("paths", nargs="*", help="explicit paths; default: ALL git-tracked files")
    args = parser.parse_args(argv)
    root = args.root.resolve()
    try:
        if args.report:
            report = args.report
            if report.is_absolute() or ".." in report.parts:
                raise CoverageError("report must be a repository-relative path")
            target = root
            for part in report.parts:
                target /= part
                if target.is_symlink():
                    raise CoverageError("report path must not contain symlinks")
            if target.exists():
                raise CoverageError("report already exists; choose a new path")
            if not target.parent.is_dir():
                raise CoverageError("report parent directory does not exist")
        policy = load_policy(root)
        records = process(root, args.paths or tracked_files(root), policy, args.command == "apply")
        counts = Counter(r["status"] for r in records)
        categories = Counter(r.get("category", "unknown") for r in records)
        if args.report:
            with (root / report).open("x", encoding="utf-8") as output:
                json.dump({
                    "counts": dict(counts), "categories": dict(categories), "files": records,
                }, output, indent=2)
                output.write("\n")
        for record in records:
            if record["status"] in ("missing", "error"):
                detail = record.get("error", record.get("diagnostic", "missing Microsoft + MIT header"))
                print(f"{record['path']}: {detail}", file=sys.stderr)
            elif args.verbose and record["status"] == "covered-without-header":
                print(f"{record['path']}: {record['category']} via {record['notice']}: {record['reason']}")
        print(
            f"Copyright coverage: {len(records)} files; "
            f"{counts['present']} headers present, {counts['applied']} applied, "
            f"{counts['covered-without-header']} explicit non-header coverage, "
            f"{counts['missing']} missing, {counts['error']} errors."
        )
        print("Coverage categories: " + ", ".join(f"{k}={v}" for k, v in sorted(categories.items())))
        print("Non-header coverage retains LICENSE/NOTICE and original ownership; use --verbose or --report for paths.")
        return 1 if counts["missing"] or counts["error"] else 0
    except (CoverageError, OSError, ValueError, subprocess.CalledProcessError) as exc:
        print(f"Copyright coverage error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
