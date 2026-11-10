# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Shared native Python supervisor. The existing Task Pod is the trust boundary.

This is not isolation from the agent identity, filesystem or local router. Only
supplied, digest-checked files are copied into a fresh invocation directory.
"""

import base64
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import time
import uuid

ARTIFACT = "kars-python-executions.json"
MAX_RECEIPTS = 65536
OUTPUT_BYTES = 2048
MAX_PROCESSES = 64
PYTHON = "/usr/bin/python3"
_active_process = None
_cancel_requested = False


def request_cancel():
    global _cancel_requested
    if _active_process is None:
        return False
    _cancel_requested = True
    return True


DESCRIPTION = (
    "Execute an exact supplied Python entrypoint in this Task's existing sandbox as the agent identity. "
    "Requires explicit python-execute, filesystem-read/write and network capabilities. No shell command "
    "or package installation tool. Copy only listed assignment inputs or files written by this run, "
    "checking their sha256; name maps each source path to a relative working filename. "
    "Existing router and Pod policy apply; this is NOT an offline or intra-agent security boundary. "
    "HTTPS_PROXY does not enable opaque HTTPS CONNECT tunnels: those are denied under governed inference. "
    "For application-made HTTPS text collection, POST JSON {url, method: 'GET'} to "
    "http://127.0.0.1:8443/egress/fetch from the application. Check the router HTTP result and returned "
    "upstream status, headers, body, truncated and observed_at; reject truncated bodies. This existing "
    "mediated endpoint retains egress and inference policy, requires no provider credentials, and returns "
    "text, not lossless binary ZIP data. Do not disable TLS verification or bypass the router. "
    "Separate http_fetch tool calls and fixture tests are not application-made live collection. "
    "Timeout 1–60 seconds, CPU 30 seconds/process, address space 512 MiB/process, file size 8 MiB, "
    "64 descriptors; stops when more than 64 invocation processes are observed. Pod PID limits remain "
    "the hard process bound. Each output stream retains 2048 bytes; "
    "overflow terminates the invocation. Actual exit, cancellation and output receipts are retained "
    "automatically in kars-python-executions.json, including failures. Fixture output is not live collection."
)
SCHEMA = {
    "name": "python_execute",
    "description": DESCRIPTION,
    "parameters": {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "entrypoint": {"type": "string", "description": "Relative .py filename in files[].name"},
            "files": {
                "type": "array",
                "minItems": 1,
                "maxItems": 32,
                "items": {
                    "type": "object",
                    "additionalProperties": False,
                    "properties": {
                        "path": {"type": "string"},
                        "name": {"type": "string"},
                        "sha256": {"type": "string", "pattern": "^sha256:[a-f0-9]{64}$"},
                    },
                    "required": ["path", "name", "sha256"],
                },
            },
            "argv": {"type": "array", "maxItems": 32, "items": {"type": "string"}},
            "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 60},
        },
        "required": ["entrypoint", "files"],
    },
}


def digest(value):
    return "sha256:" + hashlib.sha256(value).hexdigest()


def relative_name(value):
    return (
        isinstance(value, str)
        and len(value) <= 128
        and all(
            re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9._-]*", part) and part not in (".", "..") for part in value.split("/")
        )
    )


def validate(args):
    if not isinstance(args, dict) or set(args) - SCHEMA["parameters"]["properties"].keys():
        raise ValueError("Only declared Python execution arguments are accepted")
    files = args.get("files")
    if not isinstance(files, list) or not 1 <= len(files) <= 32:
        raise ValueError("Supply 1–32 assignment-owned files")
    names = set()
    for item in files:
        if (
            not isinstance(item, dict)
            or set(item) != {"path", "name", "sha256"}
            or not isinstance(item["path"], str)
            or not 0 < len(item["path"]) <= 4096
            or "\0" in item["path"]
            or not relative_name(item["name"])
            or item["name"] in names
            or not isinstance(item["sha256"], str)
            or not re.fullmatch(r"sha256:[a-f0-9]{64}", item["sha256"])
        ):
            raise ValueError("Invalid or duplicate source manifest entry")
        names.add(item["name"])
    if any("/".join(name.split("/")[:i]) in names for name in names for i in range(1, len(name.split("/")))):
        raise ValueError("Source file conflicts with a parent directory")
    if args.get("entrypoint") not in names or not args["entrypoint"].endswith(".py"):
        raise ValueError("Entrypoint must be a supplied .py file")
    if len(encoded(files)) > 8192:
        raise ValueError("Source manifest exceeds 8192 bytes")
    argv = args.get("argv", [])
    if (
        not isinstance(argv, list)
        or len(argv) > 32
        or any(not isinstance(arg, str) or len(arg.encode()) > 1024 or "\0" in arg for arg in argv)
    ):
        raise ValueError("argv must contain at most 32 bounded literal strings")
    if len(encoded(argv)) > 4096:
        raise ValueError("Argument vector exceeds 4096 bytes")
    timeout = args.get("timeout_seconds", 30)
    if type(timeout) is not int or not 1 <= timeout <= 60:
        raise ValueError("timeout_seconds must be an integer from 1 to 60")


def encoded(value):
    return json.dumps(value, ensure_ascii=True, separators=(",", ":")).encode()


def append_receipt(previous, receipt):
    value = {"version": 1, "runs": [*previous, receipt]}
    if len(encoded(value)) > MAX_RECEIPTS:
        raise ValueError("Python receipt artifact capacity exceeded")
    return encoded(value).decode()


def reserve_receipt(previous, args, identity):
    # One replacement character per retained byte bounds invalid UTF-8 in both JSON layers.
    stream = {
        "text": "\ufffd" * OUTPUT_BYTES,
        "base64": "A" * (4 * ((OUTPUT_BYTES + 2) // 3)),
        "sha256": "sha256:" + "a" * 64,
        "observed_bytes": 9007199254740991,
        "truncated": False,
    }
    worst = {
        "version": 1,
        "identity": identity,
        "entrypoint": args["entrypoint"],
        "files": args["files"],
        "argv": args.get("argv", []),
        "timeout_seconds": args.get("timeout_seconds", 30),
        "elapsed_ms": 9007199254740991,
        "reason": "cleanup_failed",
        "exit_code": -2147483648,
        "descendants_cleaned": False,
        "network": "existing-agent-governed-proxy; no claim of live collection",
        "stdout": stream,
        "stderr": stream,
        "ok": False,
    }
    current = encoded({"version": 1, "runs": previous}).decode()
    future = append_receipt(previous, worst)
    raw = len(future) - len(current)
    escaped = len(encoded(future)) - len(encoded(current)) - raw
    # Match raw and outer bounds using JSON whitespace, never a fabricated execution receipt.
    return current + " " * (raw - escaped) + "\t" * escaped


def _limits():
    # RLIMIT_NPROC counts a shared UID across containers, not this invocation.
    # Observe descendants instead; the existing Pod PID cgroup is the hard bound.
    for key, ceiling in (
        (resource.RLIMIT_CPU, 30),
        (resource.RLIMIT_AS, 512 * 1024 * 1024),
        (resource.RLIMIT_FSIZE, 8 * 1024 * 1024),
        (resource.RLIMIT_NOFILE, 64),
        (resource.RLIMIT_CORE, 0),
    ):
        _, hard = resource.getrlimit(key)
        value = ceiling if hard == resource.RLIM_INFINITY else min(ceiling, hard)
        resource.setrlimit(key, (value, value))


def _children():
    # Subreaper adoption includes double-forked children, even after setsid().
    result = set()
    pending = [os.getpid()]
    while pending:
        parent = pending.pop()
        try:
            children = Path(f"/proc/{parent}/task/{parent}/children").read_text().split()
        except FileNotFoundError:
            if parent == os.getpid():
                raise RuntimeError("Kernel descendant discovery is unavailable")
            continue
        for value in children:
            pid = int(value)
            if pid not in result:
                result.add(pid)
                pending.append(pid)
    return result


def _cleanup(process):
    deadline = time.monotonic() + 5
    while True:
        for pid in _children():
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        process.poll()
        while True:
            try:
                pid, status = os.waitpid(-1, os.WNOHANG)
                if pid == 0:
                    break
                if pid == process.pid:
                    process.returncode = os.waitstatus_to_exitcode(status)
            except ChildProcessError:
                return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.01)


def supervise(payload):
    if sys.platform != "linux" or os.getuid() != 1000:
        raise RuntimeError("Native Python requires the Linux runtime sandbox UID 1000")
    args = payload["arguments"]
    validate(args)
    contents = payload["contents"]
    if (
        not isinstance(contents, list)
        or len(contents) != len(args["files"])
        or any(not isinstance(content, str) for content in contents)
        or sum(len(content.encode()) for content in contents) > 1024 * 1024
    ):
        raise ValueError("Invalid supplied file bytes")
    for item, content in zip(args["files"], contents):
        if digest(content.encode()) != item["sha256"]:
            raise ValueError("Source digest does not match retained bytes")
    identity = payload["identity"]
    if (
        not isinstance(identity, list)
        or len(identity) != 4
        or len(encoded(identity)) > 2048
        or any(not isinstance(value, str) or not 0 < len(value) <= 256 for value in identity)
    ):
        raise ValueError("Missing assignment/call identity")
    cancelled = False

    def cancel(_sig, _frame):
        nonlocal cancelled
        cancelled = True

    signal.signal(signal.SIGTERM, cancel)
    signal.signal(signal.SIGINT, cancel)
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise RuntimeError("Kernel refused descendant supervision")
    if libc.prctl(1, signal.SIGTERM, 0, 0, 0) != 0 or os.getppid() != payload["parent_pid"]:
        raise RuntimeError("Cannot bind supervisor to its runtime parent")
    # Fail before running code if the kernel's descendant discovery is unavailable.
    _children()
    base = Path("/sandbox/.kars-python") / hashlib.sha256(encoded(identity[:3])).hexdigest()
    for parent in reversed((base, *base.parents)):
        if parent == Path("/"):
            continue
        try:
            parent.mkdir(mode=0o700)
        except FileExistsError:
            pass
        if not stat.S_ISDIR(parent.lstat().st_mode):
            raise ValueError("Execution directory is not a regular directory")
    work = base / uuid.uuid4().hex
    work.mkdir(mode=0o700)
    process = None
    started = time.monotonic()
    streams = {"stdout": bytearray(), "stderr": bytearray()}
    seen = {"stdout": 0, "stderr": 0}
    reason = "exited"
    cleaned = True

    def stop_reason():
        if cancelled:
            return "cancelled"
        if time.monotonic() - started >= args.get("timeout_seconds", 30):
            return "timeout"
        if len(_children()) > MAX_PROCESSES:
            return "process_limit"
        return None

    try:
        for item, content in zip(args["files"], contents):
            target = work / item["name"]
            target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            with target.open("xb") as file:
                file.write(content.encode())
            target.chmod(0o400)
        (work / ".home").mkdir(mode=0o700)
        (work / ".scratch").mkdir(mode=0o700)
        env = {
            "PATH": "/usr/bin:/bin",
            "HOME": str(work / ".home"),
            "TMPDIR": str(work / ".scratch"),
            "LANG": "C.UTF-8",
            "HTTP_PROXY": "http://127.0.0.1:8444",
            "HTTPS_PROXY": "http://127.0.0.1:8444",
            "http_proxy": "http://127.0.0.1:8444",
            "https_proxy": "http://127.0.0.1:8444",
            "NO_PROXY": "127.0.0.1,localhost",
            "no_proxy": "127.0.0.1,localhost",
        }
        if cancelled:
            reason = "cancelled"
        else:
            # preexec_fn runs only in this single-threaded supervisor, never in Hermes' host thread.
            process = subprocess.Popen(
                [PYTHON, "-E", "-s", "-B", "-u", str(work / args["entrypoint"]), *args.get("argv", [])],
                cwd=work,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                close_fds=True,
                start_new_session=True,
                preexec_fn=_limits,
            )
            with selectors.DefaultSelector() as poller:
                for name in streams:
                    pipe = getattr(process, name)
                    os.set_blocking(pipe.fileno(), False)
                    poller.register(pipe, selectors.EVENT_READ, name)
                while poller.get_map():
                    if stopped := stop_reason():
                        reason = stopped
                        break
                    for key, _ in poller.select(0.05):
                        data = os.read(key.fd, 4096)
                        if not data:
                            poller.unregister(key.fileobj)
                            continue
                        name = key.data
                        seen[name] += len(data)
                        streams[name].extend(data[: max(0, OUTPUT_BYTES - len(streams[name]))])
                        if seen[name] > OUTPUT_BYTES:
                            reason = "output_limit"
                    if reason != "exited":
                        break
                    if process.poll() is not None:
                        # Kill lingering descendants even when the entrypoint exits successfully.
                        cleaned = _cleanup(process)
                while process.poll() is None and reason == "exited":
                    if stopped := stop_reason():
                        reason = stopped
                        break
                    time.sleep(0.02)
    except (OSError, subprocess.SubprocessError):
        reason = "launch_failed"
    finally:
        if process is not None:
            cleaned = _cleanup(process) and cleaned
            process.stdout.close()
            process.stderr.close()
        shutil.rmtree(work)
    if not cleaned:
        reason = "cleanup_failed"
    receipt = {
        "version": 1,
        "identity": identity,
        "entrypoint": args["entrypoint"],
        "files": args["files"],
        "argv": args.get("argv", []),
        "timeout_seconds": args.get("timeout_seconds", 30),
        "elapsed_ms": round((time.monotonic() - started) * 1000),
        "reason": reason,
        "exit_code": process.returncode if process is not None else None,
        "descendants_cleaned": cleaned,
        "network": "existing-agent-governed-proxy; no claim of live collection",
    }
    for name, data in streams.items():
        receipt[name] = {
            "text": bytes(data).decode("utf-8", errors="replace"),
            "base64": base64.b64encode(data).decode(),
            "sha256": digest(data),
            "observed_bytes": seen[name],
            "truncated": seen[name] > len(data),
        }
    receipt["ok"] = reason == "exited" and receipt["exit_code"] == 0 and cleaned
    return receipt


def invoke(arguments, contents, identity, cancelled=lambda: False):
    """Host-side wrapper, also used directly by Hermes; never runs generated code here."""
    global _active_process, _cancel_requested
    validate(arguments)
    payload = {"arguments": arguments, "contents": contents, "identity": identity, "parent_pid": os.getpid()}
    process = subprocess.Popen(
        [PYTHON, "-I", "-S", "-B", str(Path(__file__).resolve())],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        close_fds=True,
        env={},
    )
    _active_process, _cancel_requested = process, False
    try:
        return _wait_for_receipt(process, payload, arguments, cancelled)
    finally:
        _active_process, _cancel_requested = None, False


def _wait_for_receipt(process, payload, arguments, cancelled):
    sent = encoded(payload)
    deadline = time.monotonic() + arguments.get("timeout_seconds", 30) + 10
    signalled = False
    while True:
        if (cancelled() or _cancel_requested) and not signalled:
            process.terminate()
            signalled = True
        try:
            output, _ = process.communicate(sent, timeout=0.1)
            break
        except subprocess.TimeoutExpired:
            sent = None
            if time.monotonic() >= deadline:
                process.terminate()
                try:
                    process.communicate(timeout=6)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.communicate()
                raise RuntimeError("Python supervisor failed to return a cleanup receipt; Task must stop")
    result = json.loads(output)
    if process.returncode != 0 or not isinstance(result, dict) or result.get("error"):
        raise RuntimeError("Python supervisor unavailable: " + str(result.get("error", "invalid result")))
    return result


if __name__ == "__main__":
    try:
        raw = sys.stdin.buffer.read(8 * 1024 * 1024 + 1)
        if len(raw) > 8 * 1024 * 1024:
            raise ValueError("Execution request too large")
        print(encoded(supervise(json.loads(raw))).decode(), flush=True)
    except Exception as error:
        print(encoded({"error": str(error)}).decode(), flush=True)
        sys.exit(1)
