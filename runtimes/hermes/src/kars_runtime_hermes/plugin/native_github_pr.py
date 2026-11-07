# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Single maintained keyless Git transaction engine, packaged by both runtimes.

Repository trees are data: never checked out, executed, or used as Git config.
Only the host wrapper supplies the private assignment root, never tool arguments.
"""

import ctypes
from functools import partial
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import resource
import signal
import stat
import subprocess
import sys
import time
import uuid

LIMIT = 2 * 1024 * 1024
TEXT_LIMIT = 128 * 1024
ORIGIN = "http://127.0.0.1:8443"
GIT = "/usr/bin/git"
SCHEMA = {
    "name": "github_pr",
    "description": (
        "Inspect one UTF-8 repository file, then create one same-repository PR "
        "editing that file through the keyless router. Requires explicit reviewed "
        "github-pr authority and a write-enabled GitHub binding. Inspect returns "
        "expected_base. Create uses that exact base and a generated create-only "
        "branch; no checkout, shell, workflow changes, merge, default-branch write, "
        "or retries. An ambiguous result must be reconciled by an operator, never "
        "replayed."
    ),
    "parameters": {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "operation": {"type": "string", "enum": ["inspect", "create"]},
            "repository": {"type": "string", "description": "Canonical owner/repo from the reviewed binding"},
            "path": {
                "type": "string",
                "description": "Repository-relative regular UTF-8 file; no .git paths or workflows",
            },
            "expected_base": {
                "type": ["string", "null"],
                "description": (
                    "Exact commit SHA observed by inspect; required for create. Omit or set null for inspect."
                ),
            },
            "content": {
                "type": ["string", "null"],
                "description": "Complete replacement UTF-8 text, required for create. Omit or set null for inspect.",
            },
            "title": {
                "type": ["string", "null"],
                "description": "PR title; required for create. Omit or set null for inspect.",
            },
            "body": {
                "type": ["string", "null"],
                "description": "PR body; required for create. Omit or set null for inspect.",
            },
        },
        "required": ["operation", "repository", "path"],
    },
}


def validate(args):
    if not isinstance(args, dict) or set(args) - SCHEMA["parameters"]["properties"].keys():
        raise ValueError("Only declared GitHub PR arguments are accepted")
    if args.get("operation") not in ("inspect", "create"):
        raise ValueError("GitHub PR operation must be inspect or create")
    repo = args.get("repository")
    if (
        not isinstance(repo, str)
        or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9._-]{0,99}", repo)
        or repo.endswith(".git")
    ):
        raise ValueError("Repository must be canonical owner/repo")
    path = args.get("path")
    if (
        not isinstance(path, str)
        or len(path) > 512
        or not all(
            re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9._-]*|\.[A-Za-z0-9_][A-Za-z0-9._-]*", part)
            and not part.lower().startswith(".git")
            and not part.endswith(".")
            for part in path.split("/")
        )
    ):
        raise ValueError("File path must be canonical and cannot contain .git configuration or workflow paths")
    if args["operation"] == "inspect":
        if any(key not in {"operation", "repository", "path"} and value is not None for key, value in args.items()):
            raise ValueError("Inspect accepts only repository and path; omit create fields or set them to null")
    else:
        if set(args) != set(SCHEMA["parameters"]["properties"]):
            raise ValueError("Create requires expected_base, content, title and body")
        if not isinstance(args["expected_base"], str) or not re.fullmatch(r"[a-f0-9]{40}", args["expected_base"]):
            raise ValueError("Create requires the inspected SHA-1 base commit")
        for key, limit in (("content", TEXT_LIMIT), ("title", 256), ("body", 65536)):
            text = args[key]
            if not isinstance(text, str) or len(text.encode("utf-8")) > limit or "\0" in text:
                raise ValueError("Invalid or oversized UTF-8 " + key)
        if not args["title"].strip() or any(ord(c) < 32 for c in args["title"]):
            raise ValueError("PR title must be nonempty single-line text")


def parent_death_signal(expected_parent):
    # Capture the owner before spawn: namespace PID 1 can be its genuine parent.
    if type(expected_parent) is not int or expected_parent < 1:
        raise ValueError("Expected host-owned parent process identity")
    if sys.platform == "linux":
        if ctypes.CDLL(None, use_errno=True).prctl(1, signal.SIGTERM, 0, 0, 0) != 0:
            raise OSError("Cannot bind Git helper lifetime")
        if os.getppid() != expected_parent:
            raise RuntimeError("Git helper parent is no longer active")


def git_child_setup(expected_parent):
    parent_death_signal(expected_parent)
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_FSIZE, (256 * 1024 * 1024, 256 * 1024 * 1024))
    resource.setrlimit(resource.RLIMIT_CPU, (90, 90))
    if sys.platform == "linux":
        resource.setrlimit(resource.RLIMIT_AS, (512 * 1024 * 1024, 512 * 1024 * 1024))


def private_directory(path):
    """Open every component without following aliases; keep the directory pinned."""
    path = Path(path)
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("Private Git workspace must be absolute and canonical")
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:]:
            try:
                os.mkdir(part, 0o700, dir_fd=fd)
            except FileExistsError:
                pass
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
        info = os.fstat(fd)
        if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o077:
            raise ValueError("Git workspace must be private to the runtime user")
        return fd
    except BaseException:
        os.close(fd)
        raise


class Engine:
    def __init__(self, root):
        self.fd = private_directory(root)
        self.root = Path(root)
        self.stage = "validation"
        self.child = None
        self.mutation = False
        self.receipt = None
        self.evidence = {}

    def close(self):
        if self.child is not None and self.child.poll() is None:
            try:
                os.killpg(self.child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            self.child.wait()
        if self.receipt is not None:
            os.close(self.receipt)
            self.receipt = None
        if hasattr(self, "work"):
            shutil.rmtree(self.work)
        os.close(self.fd)

    def api(self, repository, suffix="", payload=None):
        self.stage = "pr_create" if payload is not None else "repository_metadata"
        body = None if payload is None else json.dumps(payload).encode()
        conn = http.client.HTTPConnection("127.0.0.1", 8443, timeout=95)
        try:
            conn.request(
                "GET" if body is None else "POST",
                "/gh-api/repos/" + repository + suffix,
                body=body,
                headers={} if body is None else {"Content-Type": "application/json"},
            )
            response = conn.getresponse()
            raw = response.read(LIMIT + 1)
            if len(raw) > LIMIT:
                raise ValueError("Router GitHub response exceeds bound")
            expected = 200 if body is None else 201
            if response.status != expected:
                raise ValueError("Router GitHub HTTP " + str(response.status))
            data = json.loads(raw.decode("utf-8"))
            if not isinstance(data, dict):
                raise ValueError("Router GitHub response is not an object")
            return data
        finally:
            conn.close()

    def git(self, *args, data=None):
        env = {
            "PATH": "/usr/bin:/bin",
            "HOME": "/dev/null",
            "XDG_CONFIG_HOME": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_SYSTEM": "/dev/null",
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_ALLOW_PROTOCOL": "http",
            "GIT_NO_REPLACE_OBJECTS": "1",
            "GIT_ATTR_NOSYSTEM": "1",
            "GIT_LITERAL_PATHSPECS": "1",
            "LC_ALL": "C",
            "GIT_AUTHOR_NAME": "Kars reviewed execution",
            "GIT_AUTHOR_EMAIL": "kars@users.noreply.github.com",
            "GIT_COMMITTER_NAME": "Kars reviewed execution",
            "GIT_COMMITTER_EMAIL": "kars@users.noreply.github.com",
        }
        argv = [
            GIT,
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "credential.helper=",
            "-c",
            "http.proxy=",
            "-c",
            "http.followRedirects=false",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.http.allow=always",
            "-c",
            "submodule.recurse=false",
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "transfer.fsckObjects=true",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "fetch.unpackLimit=1",
            "-c",
            "pack.threads=1",
            "-c",
            "pack.windowMemory=32m",
            *args,
        ]
        self.child = subprocess.Popen(
            argv,
            cwd=self.work,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            preexec_fn=partial(git_child_setup, os.getpid()),
            start_new_session=True,
        )
        child = self.child
        output, errors, pending = bytearray(), bytearray(), memoryview(data or b"")
        deadline = time.monotonic() + 95
        try:
            with selectors.DefaultSelector() as selector:
                for stream in (child.stdout, child.stderr, child.stdin):
                    os.set_blocking(stream.fileno(), False)
                selector.register(child.stdout, selectors.EVENT_READ, output)
                selector.register(child.stderr, selectors.EVENT_READ, errors)
                if pending:
                    selector.register(child.stdin, selectors.EVENT_WRITE, None)
                else:
                    child.stdin.close()
                while selector.get_map():
                    if time.monotonic() > deadline:
                        raise TimeoutError("Git operation timed out")
                    for key, _ in selector.select(0.1):
                        if key.fileobj is child.stdin:
                            pending = pending[os.write(key.fd, pending[:8192]) :]
                            if not pending:
                                selector.unregister(key.fileobj)
                                child.stdin.close()
                        else:
                            chunk = os.read(key.fd, 65536)
                            if not chunk:
                                selector.unregister(key.fileobj)
                            else:
                                key.data.extend(chunk)
                                if len(key.data) > LIMIT:
                                    raise ValueError("Git response exceeds bound")
            if child.wait(timeout=max(0.01, deadline - time.monotonic())) != 0:
                raise ValueError("Git operation failed at " + self.stage)
            return bytes(output).decode("utf-8", errors="strict")
        finally:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait()
            for stream in (child.stdin, child.stdout, child.stderr):
                stream.close()
            self.child = None

    def save(self, value):
        if self.receipt is not None:
            raw = json.dumps(value).encode()
            os.lseek(self.receipt, 0, os.SEEK_SET)
            pending = memoryview(raw)
            while pending:
                pending = pending[os.write(self.receipt, pending) :]
            os.ftruncate(self.receipt, len(raw))
            os.fsync(self.receipt)

    def execute(self, args):
        validate(args)
        repo, path = args["repository"], args["path"]
        self.stage = "repository_metadata"
        metadata = self.api(repo)
        base = metadata.get("default_branch")
        if (
            not isinstance(base, str)
            or not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9._/-]{0,199}", base)
            or ".." in base
            or "//" in base
            or base.endswith(("/", ".", ".lock"))
            or metadata.get("full_name", "").lower() != repo.lower()
        ):
            raise ValueError("Repository metadata has no canonical same-repository default branch")
        name = "objects-" + uuid.uuid4().hex
        os.mkdir(name, 0o700, dir_fd=self.fd)
        # The parent root is host-owned and inaccessible to ordinary native file tools.
        self.work = self.root / name
        remote = ORIGIN + "/git/" + repo + ".git"
        self.stage = "fetch"
        self.git("init", "--bare", "--template=", "--object-format=sha1", ".")
        self.git("fetch", "--no-tags", "--no-recurse-submodules", "--depth=1", "--", remote, "refs/heads/" + base)
        commit = self.git("rev-parse", "--verify", "FETCH_HEAD^{commit}").strip()
        if not re.fullmatch(r"[a-f0-9]{40}", commit):
            raise ValueError("Invalid fetched commit")
        self.stage = "inspect"
        entry = self.git("ls-tree", "-z", commit, "--", path)
        content = None
        mode = "100644"
        if entry:
            match = re.fullmatch(r"(100644|100755) blob ([a-f0-9]{40})\t" + re.escape(path) + "\0", entry)
            if not match:
                raise ValueError("Only regular repository files may be edited")
            mode, blob = match.groups()
            size = self.git("cat-file", "-s", blob).strip()
            if not size.isdigit() or int(size) > TEXT_LIMIT:
                raise ValueError("Repository file exceeds text bound")
            content = self.git("cat-file", "blob", blob)
            if "\0" in content:
                raise ValueError("Repository file is not UTF-8 text")
        self.evidence = dict(repository=repo, path=path, base=base, expected_base=commit)
        if args["operation"] == "inspect":
            return {**self.evidence, "exists": content is not None, "content": content}
        if args["expected_base"] != commit:
            raise ValueError("Default branch changed since inspection; inspect again before creating")
        if content == args["content"]:
            raise ValueError("No file change to publish")
        self.stage = "commit"
        self.git("read-tree", commit)
        blob = self.git("hash-object", "-w", "--stdin", data=args["content"].encode()).strip()
        self.git("update-index", "--add", "--cacheinfo", mode, blob, path)
        tree = self.git("write-tree").strip()
        new_commit = self.git("commit-tree", tree, "-p", commit, data=(args["title"] + "\n").encode()).strip()
        if (
            self.git("diff-tree", "--no-commit-id", "--name-only", "-z", "-r", "--no-renames", commit, new_commit)
            != path + "\0"
        ):
            raise ValueError("Commit must change exactly the reviewed regular file")
        branch = "kars/reviewed/" + uuid.uuid4().hex
        if branch == base:
            raise ValueError("Refusing default-branch mutation")
        self.evidence.update(branch=branch, commit=new_commit)
        claim = "publication-" + hashlib.sha256(repo.lower().encode()).hexdigest() + ".json"
        self.receipt = os.open(claim, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=self.fd)
        self.stage = "push"
        self.mutation = True
        self.save({**self.evidence, "stage": self.stage, "ambiguous": True, "replay_forbidden": True})
        os.fsync(self.fd)
        ref = "refs/heads/" + branch
        # Empty expected-old lease is an atomic create-only precondition, NOT an update/force grant.
        self.git(
            "push",
            "--porcelain",
            "--no-verify",
            "--force-with-lease=" + ref + ":",
            "--",
            remote,
            new_commit + ":" + ref,
        )
        self.stage = "pr_create"
        self.save({**self.evidence, "stage": self.stage, "ambiguous": True, "replay_forbidden": True})
        result = self.api(
            repo,
            "/pulls",
            {
                "title": args["title"],
                "body": args["body"],
                "head": branch,
                "base": base,
                "maintainer_can_modify": False,
            },
        )
        number = result.get("number")
        if (
            type(number) is not int
            or number < 1
            or result.get("state") != "open"
            or result.get("html_url") != "https://github.com/" + repo + "/pull/" + str(number)
            or result.get("head", {}).get("ref") != branch
            or result.get("head", {}).get("sha") != new_commit
            or result.get("base", {}).get("ref") != base
            or any(
                result.get(side, {}).get("repo", {}).get("full_name", "").lower() != repo.lower()
                for side in ("head", "base")
            )
        ):
            raise ValueError("PR response did not confirm the requested same-repository branch and commit")
        value = {**self.evidence, "http_status": 201, "ambiguous": False, "replay_forbidden": True, "response": result}
        self.save(value)
        return value

    def run(self, args):
        try:
            return self.execute(args)
        except Exception as exc:
            value = {
                **self.evidence,
                "error": (
                    "Publication already attempted; reconcile retained receipt, never replay"
                    if isinstance(exc, FileExistsError)
                    else str(exc)
                ),
                "stage": self.stage,
                "ambiguous": self.mutation,
                "replay_forbidden": self.mutation or isinstance(exc, FileExistsError),
            }
            self.save(value)
            return value


def invoke(identity, args, cancelled):
    validate(args)
    if not isinstance(identity, list) or len(identity) != 3 or not all(isinstance(v, str) and v for v in identity):
        raise ValueError("GitHub adapter requires exact assignment identity")
    digest = hashlib.sha256(json.dumps(identity, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest()
    root = "/sandbox/.kars-github/" + digest
    if cancelled():
        raise ValueError("GitHub assignment is no longer active")
    child = subprocess.Popen(
        [sys.executable, "-I", "-S", "-B", str(Path(__file__).resolve()), str(os.getpid())],
        env={},
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    request = json.dumps({"root": root, "arguments": args}, ensure_ascii=False).encode()
    deadline = time.monotonic() + 390
    try:
        while True:
            if cancelled() or time.monotonic() >= deadline:
                raise ValueError("GitHub adapter cancelled; reconcile any dispatched mutation, never replay")
            try:
                output, _ = child.communicate(request, timeout=0.2)
                break
            except subprocess.TimeoutExpired:
                request = None
        if child.returncode != 0 or len(output) > 16 * 1024 * 1024:
            raise ValueError("GitHub adapter unavailable; any dispatched mutation is ambiguous, never replay")
        result = json.loads(output)
        if not isinstance(result, dict):
            raise ValueError("Invalid GitHub evidence; reconcile any dispatched mutation")
        return result
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


def main():
    parent_death_signal(int(sys.argv[1]) if len(sys.argv) == 2 else 0)
    request = json.loads(sys.stdin.buffer.read(512 * 1024 + 1))
    root = request["root"]
    if not isinstance(root, str) or not root.startswith("/sandbox/") or ".kars-github" not in Path(root).parts:
        raise ValueError("Expected host-owned assignment Git workspace")
    engine = Engine(root)

    def cancelled(_signum, _frame):
        raise RuntimeError("GitHub invocation cancelled; any dispatched mutation must be reconciled")

    signal.signal(signal.SIGTERM, cancelled)
    try:
        print(json.dumps(engine.run(request["arguments"])))
    finally:
        engine.close()


if __name__ == "__main__":
    main()
