# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Reviewed guards over native file, network and governed persistent memory tools.

Memory uses the existing router client only, never Hermes' local memory. Successful
submission counts as a tool call; its unchanged status determines persistence.
"""

from __future__ import annotations

import copy
import json
import re
from pathlib import Path
import stat
import time
from types import SimpleNamespace

from . import mission_protocol as wire
from . import access_request, native_github_pr, native_python
from .mission_native import FilesystemTools, read_exports, read_file_bytes
from .mission_observations import ARTIFACT
from .mission_peer import SCHEMAS as PEER_SCHEMAS, validate_call
from .mission_spill import DIRECTORY as SPILL_DIRECTORY, MAX_FILE_BYTES as MAX_SPILL_BYTES, MissionSpill

SUPPORTED_CAPABILITIES = frozenset({"filesystem-read", "filesystem-write", "network", "memory", "github-pr", "python-execute"})
MEMORY_SCHEMA = {
    "name": "foundry_memory",
    "description": "Governed persistent memory: discovered Foundry, otherwise Bridge built-in private lexical memory. The router selects the backend; failures never trigger fallback. Omit scope or use null for private memory; strings require exact grants. Search/update only; queued is not completed.",
    "parameters": {
        "type": "object",
        "properties": {
            "operation": {"type": "string", "enum": ["search", "update"]},
            "text": {"type": "string", "description": "Nonempty memory text required for update"},
            "query": {"type": "string"},
            "scope": {"type": ["string", "null"], "minLength": 1,
                      "description": "Omit or use null for the router-selected private scope. Never invent a mission scope or the literal 'private'. Strings require an exact authorized scope grant."},
            "top_k": {"type": "integer", "minimum": 1, "maximum": 2**53 - 1},
        },
        "required": ["operation"],
        "additionalProperties": False,
    },
}


def validate_memory(args):
    wire.keys(args, MEMORY_SCHEMA["parameters"]["properties"], ("operation",))
    wire.require(args["operation"] in ("search", "update"), "Memory supports search/update only")
    scope = args.get("scope")
    wire.require(scope is None or (isinstance(scope, str) and scope.strip()),
                 "Memory scope must be null or a nonempty exact authorized scope")
    wire.require(all(isinstance(args[key], str) for key in ("text", "query") if key in args),
                 "Memory text/query must be strings")
    wire.require("top_k" not in args or wire.integer(args["top_k"], 1), "Memory top_k must be a positive safe integer")
    wire.require(args["operation"] != "update" or args.get("text", "").strip(), "Memory update requires nonempty text")


NATIVE_FILES = {"read_file": "filesystem-read", "search_files": "filesystem-read",
                "write_file": "filesystem-write"}
# Pinned tools/read_extract.py includes converters, lazy installs and hosted OCR.
DOCUMENT_SUFFIXES = frozenset({
    ".ipynb", ".docx", ".xlsx", ".db", ".sqlite", ".sqlite3", ".doc", ".docm", ".ppt", ".pps", ".pot",
    ".pptx", ".pptm", ".ppsx", ".ppsm", ".xls", ".xlsm", ".xlsb", ".odt", ".ods", ".odp", ".rtf", ".epub", ".pdf",
})


COPY_GUIDANCE = (
    " Supply content OR source_path plus sha256 to copy exact retained UTF-8 bytes without model transcription. "
    "Copy requires filesystem-read and only accepts immutable inputs/<name> or this invocation's successful writes. "
    "Use the complete expected sha256:<64 lowercase hex> from the input manifest or write result. "
    "To pass through builder/QA-tested source unchanged, copy to exports/<original-name>; copying does not prove testing. "
    "Never copy ambient files, skills or runtime receipts. Existing artifact and storage limits still apply."
)


def native_write_schema(schema):
    schema = copy.deepcopy(schema)
    schema["description"] = schema.get("description", "Write a file.") + COPY_GUIDANCE
    parameters = schema["parameters"]
    parameters["properties"].update({
        "source_path": {"type": ["string", "null"], "description": "Retained input or this invocation's successful write; omit or null when supplying content"},
        "sha256": {"type": ["string", "null"], "pattern": "^sha256:[a-f0-9]{64}$", "description": "Expected source digest; required with source_path, otherwise omit or null"},
    })
    parameters["required"] = [key for key in parameters.get("required", []) if key != "content"]
    parameters["properties"]["content"]["type"] = ["string", "null"]
    parameters["properties"]["content"]["description"] = "UTF-8 content; omit or null when copying retained bytes."
    return schema


class NativeTools(FilesystemTools):
    """Keep native schemas/handlers; restrict authority to the assignment workspace."""

    def __init__(self, root, measurements, **kwargs):
        super().__init__(root, measurements, **kwargs)
        self.root = self.root.resolve()
        self.registrations = []
        self.registry = None
        self.peer_call = None
        self.peer_registrations = []
        self.memory_registration = None
        self.access_registration = None
        self.github_registration = None
        self.github_identity = None
        self.python_identity = None
        self.python_registration = None
        self.python_receipts = []
        self.owned_files = {}
        self.verified_copies = {}
        self.failed_exports = set()
        self.closed = False
        self.spill = MissionSpill(self.root)

    def install(self, registry):
        # Importing the actual module installs its native schemas and handlers.
        from tools import file_tools  # noqa: F401

        self.registry = registry
        names = {name for name, cap in NATIVE_FILES.items() if cap in self.m.phase["capabilities"]}
        if self.m.phase.get("executionMode") == "native" and self.m.phase["maxToolCalls"] > 0:
            previous = registry.snapshot_registration("kars_access_request")
            registry.register(name="kars_access_request", toolset="kars_access", schema=access_request.SCHEMA,
                              handler=lambda args, **kw: self.invoke_access(args), max_result_size_chars=192 * 1024)
            self.access_registration = (registry.get_entry("kars_access_request"), previous)
        if "python-execute" in self.m.phase["capabilities"]:
            wire.require(self.m.phase.get("executionMode") == "native" and
                         {"filesystem-read", "filesystem-write", "network"} <= set(self.m.phase["capabilities"]),
                         "Python execution requires explicit native same-agent capabilities")
            previous = registry.snapshot_registration("python_execute")
            registry.register(name="python_execute", toolset="kars_python", schema=native_python.SCHEMA,
                              handler=lambda args, **kw: self.invoke_python(args), max_result_size_chars=65536)
            self.python_registration = (registry.get_entry("python_execute"), previous)
        if "github-pr" in self.m.phase["capabilities"]:
            wire.require(self.m.phase.get("executionMode") == "native", "GitHub PR requires native execution")
            previous = registry.snapshot_registration("github_pr")
            registry.register(name="github_pr", toolset="kars_github", schema=native_github_pr.SCHEMA,
                              handler=lambda args, **kw: self.invoke_github(args), max_result_size_chars=3 * 1024 * 1024)
            self.github_registration = (registry.get_entry("github_pr"), previous)
        if "memory" in self.m.phase["capabilities"]:
            from . import foundry
            wire.require(self.m.phase.get("executionMode") == "native", "Memory requires native execution")
            previous = registry.snapshot_registration("foundry_memory")
            if registry.get_entry("foundry_memory") is None:
                registry.register(name="foundry_memory", toolset="foundry", schema=foundry._FOUNDRY_MEMORY_SCHEMA,
                                  handler=foundry._foundry_memory)
            self.memory_registration = (registry.get_entry("foundry_memory"), previous)
            names.add("foundry_memory")
        if "network" in self.m.phase["capabilities"]:
            from . import mcp_bridge
            mcp_names = {"kars_mcp_list", "kars_mcp_call"}
            if any(registry.get_entry(name) is None for name in mcp_names):
                mcp_bridge.register(SimpleNamespace(register_tool=registry.register))
            names.update(mcp_names)
            if self.peer_call is not None:
                for name, schema in PEER_SCHEMAS.items():
                    previous = registry.snapshot_registration(name)
                    registry.register(name=name, toolset="kars_mesh", schema=schema,
                                      handler=lambda args, name=name, **kw: json.dumps(self.peer_call(name, args)),
                                      max_result_size_chars=192 * 1024)
                    self.peer_registrations.append((name, registry.get_entry(name), previous))
                names.update(PEER_SCHEMAS)
        for name in sorted(names):
            entry = registry.get_entry(name)
            wire.require(entry is not None and not entry.is_async, "Required native Hermes file tool unavailable")
            self.registrations.append(entry)
            invoke = (self.invoke_memory if name == "foundry_memory" else
                      self.invoke_mcp if name.startswith("kars_mcp_") or name in PEER_SCHEMAS else self.invoke)
            schema = (MEMORY_SCHEMA if name == "foundry_memory" else
                      native_write_schema(entry.schema) if name == "write_file" else entry.schema)
            registry.register(name=name, toolset=entry.toolset,
                              schema=schema,
                              handler=lambda args, name=name, handler=entry.handler, invoke=invoke, **kw:
                                  invoke(name, handler, args, **kw), check_fn=entry.check_fn,
                              description=schema["description"] if name in ("foundry_memory", "write_file") else entry.description,
                              emoji=entry.emoji, max_result_size_chars=entry.max_result_size_chars,
                              dynamic_schema_overrides=(None if name in ("foundry_memory", "write_file")
                                                        else entry.dynamic_schema_overrides))
            if name == "foundry_memory":
                self.memory_registration = (registry.get_entry(name), self.memory_registration[1])
            if name in PEER_SCHEMAS:
                self.peer_registrations = [(n, registry.get_entry(n) if n == name else current, previous)
                                           for n, current, previous in self.peer_registrations]
        if self.python_registration is not None:
            names.add("python_execute")
        if self.github_registration is not None:
            names.add("github_pr")
        if self.access_registration is not None:
            names.add("kars_access_request")
        schemas = registry.get_definitions(names, quiet=True)
        wire.require({s["function"]["name"] for s in schemas} == names,
                     "Native Hermes file tool requirements are not installed")
        self.spill.install()
        return schemas

    def close(self):
        self.closed = True
        self.spill.close()
        for entry in self.registrations:
            if entry.name in PEER_SCHEMAS or entry.name == "foundry_memory":
                continue
            self.registry.register(name=entry.name, toolset=entry.toolset, schema=entry.schema,
                                   handler=entry.handler, check_fn=entry.check_fn,
                                   description=entry.description, emoji=entry.emoji,
                                   max_result_size_chars=entry.max_result_size_chars,
                                   dynamic_schema_overrides=entry.dynamic_schema_overrides)

        for name, current, previous in self.peer_registrations:
            self.registry.restore_registration(name, current, previous)
        if self.memory_registration is not None:
            self.registry.restore_registration("foundry_memory", *self.memory_registration)
            self.memory_registration = None
        if self.access_registration is not None:
            self.registry.restore_registration("kars_access_request", *self.access_registration)
            self.access_registration = None
        if self.python_registration is not None:
            self.registry.restore_registration("python_execute", *self.python_registration)
            self.python_registration = None
        if self.github_registration is not None:
            self.registry.restore_registration("github_pr", *self.github_registration)
            self.github_registration = None
        self.peer_call = None

    def execution_artifacts(self):
        return ({native_python.ARTIFACT: native_python.append_receipt(self.python_receipts[:-1], self.python_receipts[-1])}
                if self.python_receipts else {})

    def invoke_python(self, args):
        m = self.m
        with m.lock:
            active, self.active_call = self.active_call, None
            started, authorized, ok = time.monotonic(), False, False
            summary = None
            try:
                wire.require(not self.closed and not m.failure and active and active[1] == "python_execute"
                             and m.attempted > m.successful and m.phase.get("executionMode") == "native"
                             and "python-execute" in m.phase["capabilities"] and self.python_identity is not None,
                             "Python execution has no active explicit reviewed invocation")
                native_python.validate(args)
                wire.require(m.observations is not None and m.observations.scope is not None,
                             "Python execution requires the observed assignment scope")
                authorized = m.observations.authorize_tool(*active, args)
                wire.require(authorized, "Python execution authorization refused")
                reservation = native_python.reserve_receipt(self.python_receipts, args,
                                                            [*self.python_identity, active[0]])
                exports = read_exports(self.root / "exports")
                try:
                    wire.artifacts({**exports, native_python.ARTIFACT: reservation})
                except ValueError as error:
                    raise ValueError("Python has not started: named artifacts leave insufficient space for its execution receipt within 16 files and 128 KiB serialized total. Shorten existing exports and retry, or execute before exporting large documents. Read retained inputs directly instead of republishing them. No permissions or spending changes are needed.") from error
                owned = {self.root / "inputs" / item["name"]: item["content"] for item in self.inputs}
                owned.update(self.owned_files)
                contents = []
                for item in args["files"]:
                    path = self.checked_path(item["path"])
                    wire.require(path in owned and wire.text_hash(owned[path]) == item["sha256"],
                                 "Python source must match a retained input or this run's successful write and digest")
                    contents.append(owned[path])
                try:
                    result = native_python.invoke(args, contents, [*self.python_identity, active[0]],
                                                  lambda: self.closed or bool(m.failure))
                except Exception:
                    m.failure = "Python supervisor unavailable; Task must stop without claiming execution or cleanup"
                    raise
                self.python_receipts.append(result)
                artifact = self.execution_artifacts()[native_python.ARTIFACT]
                target = self.root / "exports" / native_python.ARTIFACT
                if target.exists():
                    target.chmod(0o600)
                target.write_text(artifact, encoding="utf-8")
                target.chmod(0o400)
                self.written.add(native_python.ARTIFACT)
                summary = {"kind": "python_execution", "receipt_sha256": wire.text_hash(native_python.encoded(result).decode()),
                           "bytes": len(native_python.encoded(result)), "reason": result["reason"], "exit_code": result["exit_code"]}
                ok = result["ok"]
                if ok:
                    m.successful += 1
                if result["reason"] == "cancelled":
                    m.failure = "Native Python execution cancelled"
                if not result["descendants_cleaned"]:
                    m.failure = "Python descendant cleanup failed; Task must stop"
                return json.dumps(result)
            except Exception as exc:
                return json.dumps({"error": str(exc), "isError": True})
            finally:
                if authorized and not m.observations.report_tool(active[0], ok, (time.monotonic() - started) * 1000,
                        summary or {"kind": "failure", "reason": "tool_execution_failed"}):
                    m.failure = "Python execution outcome receipt unavailable; do not replay"
                m.emit()

    def invoke_github(self, args):
        m = self.m
        with m.lock:
            active, self.active_call = self.active_call, None
            started, authorized, ok = time.monotonic(), False, False
            try:
                wire.require(not self.closed and not m.failure and active and active[1] == "github_pr"
                             and m.attempted > m.successful and m.phase.get("executionMode") == "native"
                             and "github-pr" in m.phase["capabilities"] and self.github_identity is not None,
                             "GitHub PR has no active explicit reviewed invocation")
                native_github_pr.validate(args)
                wire.require(m.observations is not None and m.observations.scope is not None,
                             "GitHub PR requires the observed assignment scope")
                authorized = m.observations.authorize_tool(*active, args)
                wire.require(authorized, "GitHub PR authorization refused")
                result = native_github_pr.invoke(self.github_identity, args, lambda: self.closed or bool(m.failure))
                wire.require(not self.closed and not m.failure, "GitHub invocation cancelled; reconcile any dispatched mutation")
                ok = not result.get("error")
                if ok:
                    m.successful += 1
                return json.dumps(result)
            except Exception as exc:
                return json.dumps({"error": str(exc), "isError": True})
            finally:
                if authorized and not m.observations.report_tool(active[0], ok, (time.monotonic() - started) * 1000,
                        None if ok else {"kind": "failure", "reason": "tool_execution_failed"}):
                    m.failure = "GitHub outcome receipt unavailable; do not replay the operation"
                m.emit()

    def invoke_access(self, args):
        m = self.m
        with m.lock:
            active, self.active_call = self.active_call, None
            started, authorized, ok = time.monotonic(), False, False
            try:
                wire.require(not self.closed and not m.failure and active and active[1] == "kars_access_request"
                             and m.attempted > m.successful and m.phase.get("executionMode") == "native",
                             "Access request has no active reserved invocation")
                wire.require(m.observations is not None and m.observations.scope is not None,
                             "Access request requires the observed assignment scope")
                access_request.validate(args)
                authorized = m.observations.authorize_tool(*active, args)
                wire.require(authorized, "Access request authorization refused")
                result = access_request.exchange(args, scope=m.observations.scope, request=self._fetch_request)
                wire.require(not self.closed and not m.failure, "Access request assignment is no longer active")
                ok = not result.get("error")
                if ok:
                    m.successful += 1
                return json.dumps(result)
            except Exception as exc:
                return json.dumps({"error": str(exc), "isError": True})
            finally:
                if authorized:
                    m.observations.report_tool(active[0], ok, (time.monotonic() - started) * 1000,
                                               None if ok else {"kind": "failure", "reason": "tool_execution_failed"})
                m.emit()

    def invoke_memory(self, name, handler, args, **kwargs):
        m = self.m
        with m.lock:
            active, self.active_call = self.active_call, None
            started, authorized, ok = time.monotonic(), False, False
            try:
                wire.require(not self.closed and not m.failure and active and active[1] == name == "foundry_memory"
                             and m.attempted > m.successful and m.phase.get("executionMode") == "native"
                             and "memory" in m.phase["capabilities"], "Memory tool has no reserved memory invocation")
                wire.require(m.observations is not None, "Memory tool requires router authorization")
                authorized = m.observations.authorize_tool(*active, args)
                wire.require(authorized, "Memory tool authorization refused")
                validate_memory(args)
                result = handler(args, **kwargs)
                parsed = json.loads(result)
                ok = (isinstance(parsed, dict) and not parsed.get("error") and parsed.get("isError") is not True
                      and parsed.get("success") is not False and parsed.get("ok") is not False
                      and parsed.get("status") not in ("failed", "cancelled", "canceled"))
                if ok:
                    m.successful += 1
                return result
            except Exception as exc:
                return json.dumps({"error": str(exc), "isError": True})
            finally:
                if authorized:
                    m.observations.report_tool(active[0], ok, (time.monotonic() - started) * 1000,
                                               None if ok else {"kind": "failure", "reason": "tool_execution_failed"})
                m.emit()

    def invoke_mcp(self, name, handler, args, **kwargs):
        m = self.m
        with m.lock:
            active, self.active_call = self.active_call, None
            started, authorized, ok = time.monotonic(), False, False
            try:
                wire.require(active and active[1] == name and m.attempted > m.successful
                             and "network" in m.phase["capabilities"], "MCP tool has no reserved network invocation")
                wire.require(m.observations is not None, "MCP tool requires router authorization")
                if name in PEER_SCHEMAS:
                    wire.require(self.peer_call is not None, "Native peer channel is closed")
                    validate_call(name, args)
                authorized = m.observations.authorize_tool(*active, args)
                wire.require(authorized, "Network tool authorization refused")
                schema = self.registry.get_schema(name)
                wire.keys(args, tuple(schema["parameters"]["properties"]),
                          tuple(schema["parameters"].get("required", ())))
                result = handler(args, **kwargs)
                parsed = json.loads(result)
                ok = isinstance(parsed, dict) and not parsed.get("error") and parsed.get("isError") is not True
                if ok:
                    m.successful += 1
                return result
            except Exception as exc:
                return json.dumps({"error": str(exc), "isError": True})
            finally:
                if authorized:
                    m.observations.report_tool(active[0], ok, (time.monotonic() - started) * 1000,
                                               None if ok else {"kind": "failure", "reason": "tool_execution_failed"})
                m.emit()

    def materialize_skills(self):
        for name, content in self.skills.files.items():
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            target.write_text(content, encoding="utf-8")
            target.chmod(0o400)

    def validate_inputs(self):
        for item in self.inputs:
            target = self.checked_path("inputs/" + item["name"])
            wire.require(read_file_bytes(target).decode("utf-8") == item["content"]
                         and wire.text_hash(item["content"]) == item["sha256"],
                         "Immutable input bytes do not match the supplied manifest")

    def checked_path(self, value, *, write=False, search=False):
        wire.require(isinstance(value, str) and value and len(value) < 4096, "Invalid native file path")
        path = Path(value)
        if not path.is_absolute():
            path = self.root / path
        wire.require(".." not in path.parts, "Native path cannot traverse parents")
        try:
            relative = path.relative_to(self.root)
        except ValueError:
            raise ValueError("Native file path is outside the assignment workspace") from None
        wire.require(relative.parts and relative.parts[0] in (
            "inputs", "exports", "scratch", "skills", SPILL_DIRECTORY),
                     "Use inputs/, exports/, scratch/, approved skills/, or issued tool-results/")
        spill = relative.parts[0] == SPILL_DIRECTORY
        wire.require(not spill or (not write and not search and path in self.spill.files),
                     "Native spills allow only read_file on files issued by this assignment")
        wire.require(not write or relative.parts[0] in ("exports", "scratch"), "Inputs and skills are immutable")
        for parent in (path, *path.parents):
            wire.require(not parent.is_symlink(), "Native path cannot contain symlinks")
            if parent == self.root:
                break
        if path.exists():
            mode = path.stat().st_mode
            wire.require(stat.S_ISREG(mode) or (search and stat.S_ISDIR(mode)),
                         "Native path must be a regular file or search directory")
            if path.is_file():
                wire.require(path.stat().st_size <= (MAX_SPILL_BYTES if spill else 65536),
                             "Native text file exceeds byte limit")
        if write and relative.parts[0] == "exports":
            wire.require(len(relative.parts) == 2 and wire.artifact_name(path.name) and path.name not in (ARTIFACT, native_python.ARTIFACT),
                         "Exports must be named UTF-8 artifacts; router evidence is reserved")
        return path

    def copy_content(self, args):
        wire.keys(args, ("path", "content", "source_path", "sha256"), ("path", "source_path", "sha256"))
        wire.require(args.get("content") is None, "Copy accepts source_path and sha256 instead of content")
        wire.require(self.m.phase.get("executionMode") == "native"
                     and {"filesystem-read", "filesystem-write"} <= set(self.m.phase["capabilities"]),
                     "Native copy requires filesystem-read and filesystem-write")
        wire.require(isinstance(args["sha256"], str) and re.fullmatch(r"sha256:[a-f0-9]{64}", args["sha256"]),
                     "Copy requires an explicit expected sha256")
        source = self.checked_path(args["source_path"])
        owned = {self.root / "inputs" / item["name"]: item["content"] for item in self.inputs}
        owned.update(self.owned_files)
        wire.require(source in owned and wire.text_hash(owned[source]) == args["sha256"],
                     "Copy source must match a retained immutable input or this invocation's successful write and exact sha256")
        self.validate_inputs()
        return owned[source]

    def validate_exports(self, exports):
        wire.require(not self.failed_exports, "Unresolved named output write failure")
        for path, expected in self.verified_copies.items():
            if path.parent == self.root / "exports":
                wire.require(path.name in exports and wire.text_hash(exports[path.name]) == expected,
                             "Digest-checked copy changed before artifact collection")

    def invoke(self, name, handler, args, **kwargs):
        m = self.m
        with m.lock:
            active, self.active_call = self.active_call, None
            started, authorized, ok = time.monotonic(), False, False
            summary = None
            export_name = None
            if name == "write_file" and isinstance(args.get("path"), str):
                destination = Path(args["path"])
                if not destination.is_absolute():
                    destination = self.root / destination
                if destination.parent == self.root / "exports":
                    export_name = destination.name
                    self.failed_exports.add(export_name)
                    self.written.discard(export_name)
            try:
                wire.require(active and active[1] == name and m.attempted > m.successful,
                             "Native tool has no reserved invocation")
                wire.require(m.observations is not None, "Native tool requires router authorization")
                authorized = m.observations.authorize_tool(*active, args)
                wire.require(authorized, "Native tool authorization refused")
                schema = self.registry.get_schema(name)
                wire.keys(args, tuple(schema["parameters"]["properties"]),
                          tuple(schema["parameters"].get("required", ())))
                write, search = name == "write_file", name == "search_files"
                path = self.checked_path(args.get("path", "scratch"), write=write, search=search)
                if search:
                    # Only the guarded native writer can populate these trees. Do not let
                    # backend-specific traversal follow a pre-existing link or special file.
                    for child in path.rglob("*") if path.is_dir() else ():
                        self.checked_path(str(child), search=True)
                elif write:
                    copying = args.get("source_path") is not None or args.get("sha256") is not None
                    content = self.copy_content(args) if copying else args.get("content")
                    wire.require(isinstance(content, str) and len(content.encode()) <= 65536,
                                 "Native text write exceeds byte limit")
                    if path.parent == self.root / "exports":
                        wire.artifacts({**read_exports(path.parent, exclude=path), path.name: content})
                    else:
                        existing = [p for p in (self.root / "scratch").rglob("*") if p.is_file() and p != path]
                        wire.require(len(existing) < 64 and sum(p.stat().st_size for p in existing)
                                     + len(content.encode()) <= 1024 * 1024, "Native scratch storage limit reached")
                else:
                    # No document-extraction/OCR side channel outside measured inference.
                    wire.require(path.suffix.lower() not in DOCUMENT_SUFFIXES,
                                 "Native Mission reads support UTF-8 text, not auxiliary document extraction")
                    if path in self.spill.files:
                        args = self.spill.read_arguments(path, args)
                    else:
                        read_file_bytes(path).decode("utf-8")
                handler_args = {"path": str(path), "content": content} if write and copying else {**args, "path": str(path)}
                if write:
                    handler_args.pop("source_path", None)
                    handler_args.pop("sha256", None)
                result = handler(handler_args, **kwargs)
                wire.require(isinstance(result, str), "Native file tool returned an invalid result")
                parsed = json.loads(result)
                ok = isinstance(parsed, dict) and not parsed.get("error") and parsed.get("success") is not False
                if ok:
                    self.validate_inputs()
                    if write:
                        actual = read_file_bytes(path).decode("utf-8")
                        wire.require(actual == content, "Native write did not persist requested bytes")
                        self.owned_files[path] = actual
                        if copying:
                            self.verified_copies[path] = args["sha256"]
                        else:
                            self.verified_copies.pop(path, None)
                        if export_name is not None:
                            self.failed_exports.discard(export_name)
                        parsed["sha256"] = wire.text_hash(actual)
                        result = json.dumps(parsed)
                        if path.parent == self.root / "exports":
                            self.written.add(path.name)
                            summary = {"kind": "file_write", "bytes": len(actual.encode()), "artifact_name": path.name}
                    m.successful += 1
                return result
            except Exception as exc:
                ok = False
                return json.dumps({"error": str(exc)})
            finally:
                if authorized:
                    m.observations.report_tool(active[0], ok, (time.monotonic() - started) * 1000,
                                               summary if ok else {"kind": "failure", "reason": "tool_execution_failed"})
                m.emit()
