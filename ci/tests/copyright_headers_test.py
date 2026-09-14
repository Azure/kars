#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Format and insertion-only regressions; scratch files stay in the checkout."""

import ast
import codecs
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import unittest
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("copyright_headers", ROOT / "ci/copyright_headers.py")
headers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(headers)


class HeaderTests(unittest.TestCase):
    def setUp(self):
        self.root = ROOT / (".copyright-test-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(shutil.rmtree, self.root)
        self.policy = headers.load_policy(ROOT)

    def write(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def apply(self, name, data, expected_offset=None):
        rule = headers.classification(name, self.policy)
        offset, block = headers.insertion(name, data, rule["style"])
        if expected_offset is not None:
            self.assertEqual(offset, expected_offset)
        self.assertTrue(block)
        result = data[:offset] + block + data[offset:]
        self.assertEqual(result[:offset] + result[offset + len(block):], data)
        self.assertEqual(headers.insertion(name, result, rule["style"])[1], b"")
        self.assertIn(headers.COPYRIGHT.encode(), block)
        self.assertIn(headers.LICENSE.encode(), block)
        return result

    def test_all_commentable_formats(self):
        fixtures = {
            "a.rs": b"//! Crate docs\nfn main() {}\n",
            "a.ts": b"/// <reference lib=\"dom\" />\nexport {};\n",
            "a.tsx": b"'use client';\nexport const A = () => <p />;\n",
            "a.js": b'"use strict";\nconst x = 1;',
            "a.mjs": b'"use server";\nexport const x = 1;\n',
            "a.bicep": b"param location string = 'westus'\n",
            "a.sh": b"printf 'unchanged\\n'\n",
            "a.py": b'"""Module docs."""\nx = 1\n',
            "a.toml": b"[package]\nname = 'fixture'\n",
            "a.yaml": b"---\nname: value\n",
            "a.yml": b"on: [push]\njobs: {}\n",
            "a.md": b"# Title\n\nText\n",
            "a.css": b'@import "theme.css";\na { color: red; }\n',
            "a.hbs": b"<!DOCTYPE html>\n<p>{{name}}</p>\n",
            "a.tpl": b'{{- define "test" -}}test{{- end -}}\n',
            "Dockerfile": b"FROM scratch\n",
            "Dockerfile.probe": b"FROM scratch\n",
            ".gitignore": b"!important\nbuild/\n",
            ".dockerignore": b"node_modules\n",
            "Makefile": b"test:\n\t@printf 'ok\\n'\n",
            ".github/CODEOWNERS": b"* @Azure/kars\n",
            ".env.example": b"EMPTY=\nVALUE=test\n",
            "requirements.txt": b"example==1.0\n",
            "tools/drift/allowlist-q1.txt": b"allowed_function\n",
        }
        for path, body in fixtures.items():
            with self.subTest(path=path):
                self.apply(path, body, 0)

    def test_shebangs_and_encoding_cookies(self):
        fixtures = [
            ("a.sh", b"#!/bin/sh\nprintf ok"),
            ("a.mjs", b"#!/usr/bin/env node\n'use strict';\n"),
            ("a.py", b"#!/usr/bin/env python3\n# coding: latin-1\nx = '\xe9'\n"),
            ("a.py", b"# coding=latin-1\nx = '\xe9'\n"),
            ("a.py", b"# Original attribution\n# -*- coding: latin-1 -*-\nx = '\xe9'\n"),
            ("a.py", b"#!/usr/bin/python3\n\nx = 1\n"),
        ]
        for name, body in fixtures:
            with self.subTest(body=body):
                expected_lines = 2 if b"coding" in body.splitlines()[1] else 1
                prefix = b"".join(body.splitlines(keepends=True)[:expected_lines])
                after = self.apply(name, body, len(prefix))
                if name.endswith(".py"):
                    self.assertEqual(ast.dump(ast.parse(body)), ast.dump(ast.parse(after)))
        # A cookie-looking comment after executable code is not an encoding
        # declaration; the header must still precede the executable statement.
        self.apply("a.py", b"x = 1\n# coding: utf-8\n", 0)

    def test_bom_crlf_and_no_final_newline(self):
        for body in (
            codecs.BOM_UTF8 + b'"use client";\r\nexport {};',
            b"const x = 1;\r\n\r\n",
            b"const x = 1;",
            b"",
        ):
            with self.subTest(body=body):
                after = self.apply("a.ts", body)
                if body.startswith(codecs.BOM_UTF8):
                    self.assertTrue(after.startswith(codecs.BOM_UTF8))
                if b"\r\n" in body:
                    self.assertNotIn(b"\n", after.replace(b"\r\n", b""))
                self.assertTrue(after.endswith(body[3:] if body.startswith(codecs.BOM_UTF8) else body))

    def test_rust_inner_attributes_are_not_shebangs(self):
        self.apply("a.rs", b"#![no_std]", 0)
        self.apply("a.rs", b"#![deny(unsafe_code)]\n//! Crate docs\n", 0)
        existing = b"#![no_main]\n" + headers.STYLES["slash"].encode() + b"use libfuzzer_sys::fuzz_target;\n"
        self.assertEqual(headers.insertion("a.rs", existing, "slash")[1], b"")

    def test_docker_directives_preserved(self):
        for directives in (
            b"# syntax=docker/dockerfile:1.7\n",
            b"# syntax=docker/dockerfile:1.7\r\n# escape=`\r\n# check=skip=JSONArgsRecommended\r\n",
            b"# SYNTAX=docker/dockerfile:1\n# ESCAPE=\\\n",
        ):
            after = self.apply("Dockerfile.dev", directives + b"\nFROM scratch\n", len(directives))
            self.assertTrue(after.startswith(directives))
        self.apply("Dockerfile", b"# explanation\n# syntax=not-a-directive\nFROM scratch\n", 0)

    def test_markdown_frontmatter_and_existing_html_notice(self):
        for prefix in (
            b"---\nname: skill\nmetadata: {a: b}\n---\n",
            b"---\r\nname: skill\r\n...\r\n",
            b"+++\nname = 'skill'\n+++\n",
        ):
            self.apply("SKILL.md", prefix + b"\n# Heading\n", len(prefix))
        body = f"<!--\n{headers.COPYRIGHT}\n{headers.LICENSE}\n-->\n\n# Title".encode()
        self.assertEqual(headers.insertion("a.md", body, "html")[1], b"")

    def test_css_charset_and_import(self):
        prefix = b'@charset "UTF-8";'
        self.apply("a.css", prefix + b'\n@import "theme.css";\n', len(prefix))

    def test_template_headers_never_emit_or_trim_whitespace(self):
        for path in ("chart/templates/config.yaml", "chart/templates/NOTES.txt", "a.tpl", "a.hbs"):
            for body in (
                b'{{- if .Values.enabled -}}\nkey: value\n{{- end -}}\n',
                b'  leading whitespace\n{{- /* existing comment */ -}}\n',
                b"plain text with trailing spaces  \n\n",
            ):
                with self.subTest(path=path, body=body):
                    after = self.apply(path, body, 0)
                    self.assertTrue(after.endswith(body))
                    marker = b"--}}" if path.endswith(".hbs") else b"*/}}"
                    self.assertEqual(after.split(marker, 1)[1], body)

    def test_original_attribution_and_legacy_annotation_preserved(self):
        body = b"// Copyright (c) 2026 Original Author\n// SPDX-License-Identifier: MIT\nfn main() {}\n"
        after = self.apply("a.rs", body)
        self.assertEqual(after.count(b"Original Author"), 1)
        self.assertTrue(after.endswith(body))
        legacy = (
            f"// {headers.COPYRIGHT}\n// ci:loc-ok existing annotation\n\n"
            f"// {headers.LICENSE}\n\nfn main() {{}}\n"
        ).encode()
        self.assertEqual(headers.insertion("a.rs", legacy, "slash")[1], b"")

    def test_both_notices_required_in_leading_comments(self):
        for body in (
            f"// {headers.COPYRIGHT}\nfn main() {{}}\n".encode(),
            f'const text = "// {headers.COPYRIGHT}\\n// {headers.LICENSE}";\n'.encode(),
            f"fn main() {{}}\n// {headers.COPYRIGHT}\n// {headers.LICENSE}\n".encode(),
            f"// {headers.COPYRIGHT}\nfn main() {{}}\n// {headers.LICENSE}\n".encode(),
        ):
            self.assertTrue(headers.insertion("a.rs", body, "slash")[1])

    def test_unknown_and_unsafe_formats_fail(self):
        for name in ("unknown.conf", "new.txt", "new.lock", "own.whl", "unknown", "a.cjs"):
            with self.subTest(name=name):
                with self.assertRaises(headers.CoverageError):
                    headers.classification(name, self.policy)
        for name, data in (
            ("a.md", b"---\nname: unfinished\n"),
            ("a.py", b"# coding: not-an-encoding\n"),
            ("a.sh", b"#!/bin/sh"),
            ("Dockerfile", b"# syntax=docker/dockerfile:1"),
            ("a.ts", b"\x00binary"),
            ("a.yaml", b"\xffinvalid"),
            ("a.css", b'@charset "UTF-8"'),
        ):
            with self.subTest(name=name, data=data):
                with self.assertRaises(headers.CoverageError):
                    headers.insertion(name, data, headers.classification(name, self.policy)["style"])

    def test_non_header_coverage_never_changes_bytes(self):
        fixtures = {
            "data.json": b'{"signature":"unchanged"}\n',
            "Cargo.lock": b"# generated\nversion = 4\n",
            "drawing.excalidraw": b'{"type":"excalidraw"}',
            "asset.png": b"\x89PNG\r\n\x00",
            "asset.gif": b"GIF89a\x00",
            "asset.ico": b"\x00icon",
            "asset.svg": b"<svg/>",
            "slide.pptx": b"PK\x00",
            "record.cast": b'{"version":2}\n[1.0,"o","record"]\n',
            "a2a-gateway/testdata/test-cert.pem": b"certificate bytes",
            "vendor/sandbox-wheels/external.whl": b"PK\x00",
            "vendor/agt/external.tgz": b"\x1f\x8barchive",
            "vendor/agt/SHA256SUMS": b"original digest  external.tgz\n",
            "vendor/external.rs": b"// Upstream copyright\n",
            "docs/site/mermaid-init.js": b"// MPL upstream\n",
            "bridge/web/public/next.svg": b"<svg/>",
            "bridge/web/src/app/favicon.ico": b"\0icon",
            "cli/blocklists/seed-domains.txt": b"# upstream\nexample.test\n",
            "deploy/helm/kars/files/kars-default-agt-profile.yaml": b"name: literal-embedded-value\n",
            "tools/e2e-harness/scenarios/exec-brief/prompt.txt": b"Prompt input.\n",
            "tools/headlamp-plugin/dist/main.js": b"minified();",
            "tools/headlamp-plugin/dist/package.json": b'{"name":"generated-manifest"}\n',
            "LICENSE": b"Original legal text\n",
            "NOTICE": b"Original attribution\n",
            "THIRD_PARTY_NOTICES.txt": b"Original third-party license\n",
        }
        for path, data in fixtures.items():
            self.write(path, data)
        results = headers.process(self.root, list(fixtures), self.policy, apply=True)
        self.assertTrue(all(r["status"] == "covered-without-header" for r in results))
        for path, data in fixtures.items():
            self.assertEqual((self.root / path).read_bytes(), data)
        with self.assertRaises(headers.CoverageError):
            headers.classification("bridge/unknown-format.xyz", self.policy)

    def test_reported_generated_bypasses_fail_closed(self):
        fixtures = {
            "cli/src/build/handwritten.ts": b'"use client";\nexport const value = 1;\n',
            "cli/src/authored.d.ts": b'/// <reference lib="dom" />\nexport declare const value: string;\n',
            "ci/tests/coverage/unknown.newformat": b"unrecognized first-party input\n",
        }
        for name, body in fixtures.items():
            self.write(name, body)
        results = headers.process(self.root, list(fixtures), self.policy, apply=True)
        by_path = {r["path"]: r for r in results}
        for name in list(fixtures)[:2]:
            self.assertEqual(by_path[name]["category"], "header")
            self.assertEqual(by_path[name]["status"], "missing")
        unknown = by_path["ci/tests/coverage/unknown.newformat"]
        self.assertEqual(unknown["status"], "error")
        self.assertIn("unknown format", unknown["error"])
        for name, body in fixtures.items():
            self.assertEqual((self.root / name).read_bytes(), body)
        sources = list(fixtures)[:2]
        results = headers.process(self.root, sources, self.policy, apply=True)
        self.assertTrue(all(r["status"] == "applied" for r in results))
        for name in sources:
            self.assertEqual(
                (self.root / name).read_bytes(),
                headers.STYLES["slash"].encode() + fixtures[name],
            )
        again = headers.process(self.root, sources, self.policy, apply=True)
        self.assertTrue(all(r["status"] == "present" for r in again))

    def test_output_directory_names_never_imply_generated_coverage(self):
        directories = ("build", "target", "dist", "coverage", ".turbo", "node_modules")
        for directory in directories:
            for prefix in ("", "cli/src/", "ci/tests/fixtures/"):
                with self.subTest(directory=directory, prefix=prefix):
                    for filename in ("handwritten.ts", "authored.d.ts"):
                        path = f"{prefix}{directory}/{filename}"
                        self.assertEqual(headers.classification(path, self.policy)["category"], "header")
                        self.apply(path, b"export declare const value: string;\n", 0)
                    with self.assertRaises(headers.CoverageError):
                        headers.classification(f"{prefix}{directory}/unknown.newformat", self.policy)
                    self.assertEqual(
                        headers.classification(f"{prefix}{directory}/data.json", self.policy)["category"],
                        "repository-license",
                    )
        nested = "cli/src/" + "/".join(directories) + "/handwritten.ts"
        self.assertEqual(headers.classification(nested, self.policy)["category"], "header")
        self.apply(nested, b"export const value = 1;\n", 0)

    def test_generated_coverage_requires_exact_reviewed_paths(self):
        expected = {"tools/headlamp-plugin/dist/main.js", "tools/headlamp-plugin/dist/package.json"}
        generated = {p for p, r in self.policy["files"].items() if r["category"] == "generated"}
        self.assertEqual(generated, expected)
        for name in expected:
            rule = headers.classification(name, self.policy)
            self.assertEqual(rule["notice"], "NOTICE")
            self.assertIn("tools/headlamp-plugin/package.json", rule["reason"])
        for name in (
            "tools/headlamp-plugin/dist/authored.ts",
            "tools/headlamp-plugin/dist/authored.d.ts",
            "examples/tools/headlamp-plugin/dist/main.js",
            "tools/headlamp-plugin/dist/nested/main.js",
        ):
            self.assertEqual(headers.classification(name, self.policy)["category"], "header")
        with self.assertRaises(headers.CoverageError):
            headers.classification("tools/headlamp-plugin/dist/main.js.newformat", self.policy)
        name = "ci/tests/fixtures/generated/types.d.ts"
        self.policy["files"][name] = {
            "category": "generated", "notice": "NOTICE",
            "reason": "Reviewed declaration fixture emitted by this test generator.",
        }
        body = b"declare const generated: string;\n"
        path = self.write(name, body)
        for _ in range(2):
            result, = headers.process(self.root, [name], self.policy, apply=True)
            self.assertEqual(result["status"], "covered-without-header")
            self.assertEqual(path.read_bytes(), body)

    def test_generated_rules_cannot_be_format_wide(self):
        for name in ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.txt"):
            self.write(name, b"notice\n")
        policy_path = self.write(headers.POLICY_PATH, json.dumps(self.policy).encode())
        headers.load_policy(self.root)
        self.policy["formats"][".d.ts"] = {
            "category": "generated", "notice": "NOTICE", "reason": "Not a reviewed exact file.",
        }
        policy_path.write_text(json.dumps(self.policy))
        with self.assertRaises(headers.CoverageError):
            headers.load_policy(self.root)

    def test_verbatim_helm_policy_preserves_raw_byte_digest(self):
        name = "deploy/helm/kars/files/kars-default-agt-profile.yaml"
        original = (ROOT / name).read_bytes()
        path = self.write(name, original)

        def digest(body):
            # The controller and router use this filename/body wire contract.
            filename = b"agt-profile.yaml"
            canonical = (
                len(filename).to_bytes(8, "big") + filename
                + len(body).to_bytes(8, "big") + body
            )
            return hashlib.sha256(canonical).hexdigest()

        expected = digest(original)
        result, = headers.process(self.root, [name], self.policy, apply=True)
        self.assertEqual(result["status"], "covered-without-header")
        self.assertEqual(path.read_bytes(), original)
        self.assertEqual(digest(path.read_bytes()), expected)
        self.assertNotEqual(digest(headers.header_for("hash", original) + original), expected)

    def test_process_preserves_mode_and_proves_insertion(self):
        body = b"#!/bin/sh\r\nprintf 'same\\n'\r\n"
        path = self.write("space in name.sh", body)
        path.chmod(0o751)
        result, = headers.process(self.root, ["space in name.sh"], self.policy, apply=True)
        after = path.read_bytes()
        self.assertEqual(result["status"], "applied")
        self.assertEqual(path.stat().st_mode & 0o777, 0o751)
        self.assertEqual(result["before_sha256"], hashlib.sha256(body).hexdigest())
        self.assertEqual(result["after_sha256"], hashlib.sha256(after).hexdigest())
        offset, length = result["offset"], result["inserted_bytes"]
        self.assertEqual(after[:offset] + after[offset + length:], body)
        again, = headers.process(self.root, ["space in name.sh"], self.policy, apply=True)
        self.assertEqual(again["status"], "present")
        self.assertEqual(path.read_bytes(), after)

    def test_errors_prevent_partial_application(self):
        good = self.write("good.py", b"value = 1\n")
        self.write("unknown.format", b"unknown\n")
        results = headers.process(self.root, ["good.py", "unknown.format"], self.policy, apply=True)
        self.assertEqual(good.read_bytes(), b"value = 1\n")
        self.assertEqual({r["status"] for r in results}, {"missing", "error"})
        self.write("linked.py", b"value = 2\n")
        (self.root / "link.py").symlink_to("linked.py")
        for name in ("missing.py", "link.py", "../escape.py"):
            result, = headers.process(self.root, [name], self.policy, apply=True)
            self.assertEqual(result["status"], "error")

    def test_policy_schema_errors(self):
        for name in ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.txt"):
            self.write(name, b"notice\n")
        policy_path = self.write(headers.POLICY_PATH, b"{}")
        for value in (b"{}", b"null", b"[]"):
            policy_path.write_bytes(value)
            with self.assertRaises(headers.CoverageError):
                headers.load_policy(self.root)
        policy = json.loads(json.dumps(self.policy))
        policy["files"]["escape"] = {"category": "ignored"}
        policy_path.write_text(json.dumps(policy))
        with self.assertRaises(headers.CoverageError):
            headers.load_policy(self.root)

    def test_cli_tracks_every_file_reports_exceptions_and_is_idempotent(self):
        for name in ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.txt"):
            self.write(name, b"notice\n")
        self.write(headers.POLICY_PATH, json.dumps(self.policy).encode())
        self.write("new.py", b"value = 1\n")
        self.write("data.json", b"{}")
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(headers.main(["check", "--root", str(self.root)]), 1)
            self.assertEqual(headers.main(["apply", "--root", str(self.root), "--report", "report.json"]), 0)
            self.assertEqual(headers.main(["check", "--root", str(self.root)]), 0)
            self.assertEqual(headers.main(["apply", "--root", str(self.root)]), 0)
            self.assertEqual(headers.main(["apply", "--root", str(self.root), "--report", "../escape.json"]), 2)
            self.assertEqual(headers.main(["apply", "--root", str(self.root), "--report", "report.json"]), 2)
            self.assertEqual(headers.main(["apply", "--root", str(self.root), "--report", "missing/report.json"]), 2)
        report = json.loads((self.root / "report.json").read_bytes())
        self.assertEqual(len(report["files"]), 6)
        self.assertEqual(report["counts"]["applied"], 1)
        self.assertEqual(report["counts"]["covered-without-header"], 5)
        self.write("ci/tests/coverage/unknown.newformat", b"must not be silently ignored\n")
        subprocess.run(["git", "add", "ci/tests/coverage/unknown.newformat"], cwd=self.root, check=True)
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(headers.main(["check", "--root", str(self.root)]), 1)

    def test_python_ast_and_toml_parser_equivalence(self):
        import tomllib
        python = b'#!/usr/bin/python3\n"""Module docs."""\nfrom __future__ import annotations\nx: int = 1\n'
        after = self.apply("a.py", python)
        self.assertEqual(ast.dump(ast.parse(python)), ast.dump(ast.parse(after)))
        toml = b"[package]\nname = 'fixture'\nitems = ['a', 'b']\n"
        self.assertEqual(tomllib.loads(toml.decode()), tomllib.loads(self.apply("a.toml", toml).decode()))

    @unittest.skipUnless(shutil.which("node"), "Node not installed")
    def test_javascript_directives_remain_executable_prologue(self):
        for directive in (b'"use client";\n', b'"use server";\n'):
            body = b'#!/usr/bin/env node\n' + directive + b'"use strict";\nconsole.log((function () { return this === undefined; })());\n'
            path = self.write("directive.mjs", body)
            before = subprocess.check_output(["node", str(path)])
            path.write_bytes(self.apply("directive.mjs", body))
            after = subprocess.check_output(["node", str(path)])
            self.assertEqual(before, b"true\n")
            self.assertEqual(before, after)

    @unittest.skipUnless(shutil.which("node"), "Node not installed")
    def test_frontend_ast_and_css_parser_equivalence_when_installed(self):
        program = r"""
const fs = require("node:fs");
let ts, css;
try {
  const paths = [process.argv[1]];
  ts = require(require.resolve("typescript", { paths }));
  css = require(require.resolve("postcss", { paths }));
} catch { process.exit(77); }
const fixtures = JSON.parse(fs.readFileSync(0, "utf8"));
function structure(node) {
  return [node.kind, node.text ?? null, node.getChildren().map(structure)];
}
for (const [name, before, after] of fixtures) {
  let original, updated;
  if (name.endsWith(".css")) {
    function cssStructure(node) {
      if (node.type === "comment") return null;
      return [node.type, node.name, node.params, node.selector, node.prop,
        node.value, node.important, node.nodes?.map(cssStructure).filter(Boolean)];
    }
    original = cssStructure(css.parse(before));
    updated = cssStructure(css.parse(after));
  } else {
    function parse(text) {
      const source = ts.createSourceFile(name, text, ts.ScriptTarget.Latest, true);
      if (source.parseDiagnostics.length) throw new Error("invalid fixture");
      return [source.statements.map(structure),
        source.libReferenceDirectives.map(reference => reference.fileName)];
    }
    original = parse(before);
    updated = parse(after);
  }
  if (JSON.stringify(original) !== JSON.stringify(updated)) throw new Error(name);
}
"""
        fixtures = []
        for name, body in (
            ("client.tsx", b'"use client";\nexport const App = () => <p>Hello</p>;\n'),
            ("server.ts", b'"use server";\nexport async function action() { return 1; }\n'),
            ("refs.ts", b'/// <reference lib="dom" />\nexport {};\n'),
            ("authored.d.ts", b'/// <reference lib="dom" />\nexport declare const value: string;\n'),
            ("strict.js", b'"use strict";\nfunction value() { return this; }\n'),
            ("import.css", b'@import "theme.css";\np { color: red !important; }\n'),
            ("charset.css", b'@charset "UTF-8";\n@import "theme.css";\n'),
        ):
            fixtures.append([name, body.decode(), self.apply(name, body).decode()])
        result = subprocess.run(
            ["node", "-e", program, str(ROOT / "cli")],
            input=json.dumps(fixtures).encode(), capture_output=True,
        )
        if result.returncode == 77:
            self.skipTest("Existing TypeScript/PostCSS dependencies are not installed")
        self.assertEqual(result.returncode, 0, result.stderr.decode())

    def test_yaml_parser_equivalence_when_installed(self):
        try:
            import yaml
        except ImportError:
            self.skipTest("PyYAML is not installed")
        body = b"---\non: [push]\njobs: {}\n---\nvalue: |\n  exact string\n"
        self.assertEqual(
            list(yaml.safe_load_all(body)),
            list(yaml.safe_load_all(self.apply("workflow.yml", body))),
        )

    @unittest.skipUnless(shutil.which("make"), "Make not installed")
    def test_make_execution_equivalence(self):
        body = b"all:\n\t@printf 'unchanged\\n'\n"
        path = self.write("Makefile", body)
        before = subprocess.check_output(["make", "-s", "-f", str(path)])
        path.write_bytes(self.apply("Makefile", body))
        self.assertEqual(subprocess.check_output(["make", "-s", "-f", str(path)]), before)

    @unittest.skipUnless(shutil.which("bash"), "Bash not installed")
    def test_shell_execution_equivalence(self):
        body = b"#!/bin/sh\nvalue='literal'\nprintf '%s\\n' \"$value\"\n"
        path = self.write("test.sh", body)
        before = subprocess.check_output(["bash", str(path)])
        path.write_bytes(self.apply("test.sh", body))
        subprocess.run(["bash", "-n", str(path)], check=True)
        self.assertEqual(subprocess.check_output(["bash", str(path)]), before)

    @unittest.skipUnless(shutil.which("helm"), "Helm not installed")
    def test_real_helm_render_equivalence_including_trimmed_comments(self):
        fixtures = {
            "chart/Chart.yaml": b"apiVersion: v2\nname: fixture\nversion: 0.1.0\n",
            "chart/values.yaml": b"enabled: true\n",
            "chart/templates/_helpers.tpl": b'{{- define "fixture.name" -}}example{{- end -}}\n',
            "chart/templates/config.yaml": (
                b'{{- if .Values.enabled -}}\napiVersion: v1\nkind: ConfigMap\n'
                b'metadata:\n  name: {{ include "fixture.name" . }}\n'
                b'data:\n  value: unchanged\n{{- end -}}\n'
            ),
            "chart/templates/NOTES.txt": b'  Installed {{ include "fixture.name" . }}.\n',
        }
        for path, data in fixtures.items():
            self.write(path, data)
        command = ["helm", "template", "fixture", str(self.root / "chart"), "--render-subchart-notes"]
        before = subprocess.check_output(command)
        for path, data in fixtures.items():
            (self.root / path).write_bytes(self.apply(path, data))
        self.assertEqual(subprocess.check_output(command), before)


if __name__ == "__main__":
    unittest.main()
