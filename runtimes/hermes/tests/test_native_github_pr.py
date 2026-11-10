# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Local Git/HTTP boundary tests, never live GitHub publication."""

import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import threading
from types import SimpleNamespace
from urllib.parse import urlsplit

import pytest

from kars_runtime_hermes.plugin import native_github_pr as adapter

REPO = "owner/repo"
INSPECT = {"operation": "inspect", "repository": REPO, "path": "README.md"}


@pytest.fixture
def upstream(tmp_path, monkeypatch):
    repository = tmp_path / "remote" / "owner" / "repo.git"
    repository.mkdir(parents=True)
    env = {
        "PATH": "/usr/bin:/bin",
        "HOME": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_AUTHOR_NAME": "Test",
        "GIT_AUTHOR_EMAIL": "test@example.invalid",
        "GIT_COMMITTER_NAME": "Test",
        "GIT_COMMITTER_EMAIL": "test@example.invalid",
    }

    def git(*args, data=None):
        return (
            subprocess.run(
                [adapter.GIT, *args],
                cwd=repository,
                env=env,
                input=data,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )
            .stdout.decode()
            .strip()
        )

    git("init", "--bare", "--template=", ".")
    git("config", "http.receivepack", "true")
    files = {
        "README.md": "Original documentation\n",
        "keep.txt": "Unchanged\n",
        ".gitattributes": "* filter=hostile\n",
        ".github/workflows/keep.yml": "name: unchanged\n",
    }
    for name, text in files.items():
        blob = git("hash-object", "-w", "--stdin", data=text.encode())
        git("update-index", "--add", "--cacheinfo", "100644", blob, name)
    blob = git("hash-object", "-w", "--stdin", data=b"/outside")
    git("update-index", "--add", "--cacheinfo", "120000", blob, "alias")
    tree = git("write-tree")
    initial = git("commit-tree", tree, data=b"Initial fixture\n")
    git("update-ref", "refs/heads/main", initial)
    git("symbolic-ref", "HEAD", "refs/heads/main")
    state = SimpleNamespace(
        git=git,
        initial=initial,
        repository=repository,
        calls=[],
        posts=[],
        metadata_status=200,
        post_status=201,
        drop=False,
        invalid=False,
        collision=False,
        collision_sha=None,
    )

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            self.handle_request()

        def do_POST(self):
            self.handle_request()

        def handle_request(self):
            state.calls.append((self.command, self.path))
            body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            assert self.headers.get("Authorization") is None
            if self.path.startswith("/gh-api/"):
                if self.command == "GET":
                    status = state.metadata_status
                    result = {"full_name": REPO, "default_branch": getattr(state, "default_branch", "main")}
                else:
                    assert self.path == "/gh-api/repos/owner/repo/pulls"
                    payload = json.loads(body)
                    state.posts.append(payload)
                    if state.drop:
                        self.close_connection = True
                        return
                    status = state.post_status
                    result = {
                        "number": 17,
                        "state": "open",
                        "html_url": "https://github.com/owner/repo/pull/17",
                        "head": {
                            "ref": payload["head"],
                            "sha": git("rev-parse", "refs/heads/" + payload["head"]),
                            "repo": {"full_name": REPO},
                        },
                        "base": {"ref": payload["base"], "repo": {"full_name": REPO}},
                    }
                    if state.invalid:
                        result["head"]["repo"]["full_name"] = "other/repo"
                raw = json.dumps(result).encode()
                self.send_response(status)
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)
                return
            parsed = urlsplit(self.path)
            assert parsed.path.startswith("/git/owner/repo.git/")
            if state.collision and parsed.query == "service=git-receive-pack":
                git("update-ref", "refs/heads/kars/reviewed/" + "a" * 32, initial)
                state.collision_sha = initial
            cgi_env = {
                **env,
                "GIT_PROJECT_ROOT": str(repository.parent.parent),
                "GIT_HTTP_EXPORT_ALL": "1",
                "REQUEST_METHOD": self.command,
                "PATH_INFO": parsed.path.removeprefix("/git"),
                "QUERY_STRING": parsed.query,
                "CONTENT_TYPE": self.headers.get("Content-Type", ""),
                "CONTENT_LENGTH": str(len(body)),
                "REMOTE_USER": "fixture",
            }
            response = subprocess.run(
                [adapter.GIT, "http-backend"],
                env=cgi_env,
                input=body,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            ).stdout
            headers, raw = response.split(b"\r\n\r\n", 1)
            entries = [line.decode().split(":", 1) for line in headers.split(b"\r\n")]
            self.send_response(next((int(value.split()[0]) for name, value in entries if name == "Status"), 200))
            for name, value in entries:
                if name != "Status":
                    self.send_header(name, value.strip())
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    worker = threading.Thread(target=server.serve_forever)
    worker.start()
    original_connection = http.client.HTTPConnection

    def connection(host, port, **kwargs):
        assert (host, port) == ("127.0.0.1", 8443)
        return original_connection(host, server.server_port, **kwargs)

    monkeypatch.setattr(adapter.http.client, "HTTPConnection", connection)
    monkeypatch.setattr(adapter, "ORIGIN", f"http://127.0.0.1:{server.server_port}")
    try:
        yield state
    finally:
        server.shutdown()
        server.server_close()
        worker.join()


def run(root, args):
    engine = adapter.Engine(root)
    try:
        return engine.run(args)
    finally:
        engine.close()


def create_args(upstream, **overrides):
    return {
        **INSPECT,
        "operation": "create",
        "expected_base": upstream.initial,
        "content": "Reviewed documentation: café 🌍\n",
        "title": "Improve documentation",
        "body": "One reviewed file.",
        **overrides,
    }


@pytest.mark.parametrize("nullable_fields", [False, True])
def test_real_keyless_git_inspect_publish_and_replay_receipt(tmp_path, upstream, nullable_fields):
    root = tmp_path / "private"
    args = {
        **INSPECT,
        **({key: None for key in ("expected_base", "content", "title", "body")} if nullable_fields else {}),
    }
    observed = run(root, args)
    assert observed["content"] == "Original documentation\n"
    assert observed["expected_base"] == upstream.initial
    assert list(root.iterdir()) == []
    result = run(root, create_args(upstream))
    assert "error" not in result, result
    assert result["http_status"] == 201 and not result["ambiguous"] and result["replay_forbidden"]
    assert result["response"]["html_url"] == "https://github.com/owner/repo/pull/17"
    branch = result["branch"]
    assert branch.startswith("kars/reviewed/")
    assert upstream.git("rev-parse", "refs/heads/main") == upstream.initial
    assert upstream.git("show", branch + ":README.md") == "Reviewed documentation: café 🌍"
    assert upstream.git("diff-tree", "--no-commit-id", "--name-only", "-r", upstream.initial, branch) == "README.md"
    assert upstream.git("show", branch + ":.github/workflows/keep.yml") == "name: unchanged"
    assert upstream.posts == [
        {
            "title": "Improve documentation",
            "body": "One reviewed file.",
            "head": branch,
            "base": "main",
            "maintainer_can_modify": False,
        }
    ]
    receipt = next(root.glob("publication-*.json"))
    assert json.loads(receipt.read_text()) == result
    assert list(root.iterdir()) == [receipt]
    repeated = run(root, create_args(upstream))
    assert repeated["replay_forbidden"] and "already attempted" in repeated["error"]
    assert len(upstream.posts) == 1 and json.loads(receipt.read_text()) == result


@pytest.mark.parametrize("key", ["expected_base", "content", "title", "body"])
def test_strict_provider_placeholders_are_nullable_only_for_inspect(key):
    assert adapter.SCHEMA["parameters"]["properties"][key]["type"] == ["string", "null"]
    adapter.validate({**INSPECT, key: None})
    for value in ("", "ignored", 0, False, {}, []):
        with pytest.raises(ValueError):
            adapter.validate({**INSPECT, key: value})
    create = {
        **INSPECT,
        "operation": "create",
        "expected_base": "a" * 40,
        "content": "Reviewed documentation",
        "title": "Docs",
        "body": "Review",
        key: None,
    }
    with pytest.raises(ValueError):
        adapter.validate(create)
    with pytest.raises(ValueError):
        adapter.validate({**INSPECT, "branch": None})


@pytest.mark.parametrize(
    "path",
    [
        "../x",
        "/x",
        "a//b",
        "a/../b",
        "-c",
        ".git/config",
        ".GIT/config",
        ".github/workflows/build.yml",
        ".gitattributes",
        ".gitmodules",
        "a/.git/config",
        "a\\b",
        "a\nb",
        "file.",
        ":(glob)*",
    ],
)
def test_path_bypasses_rejected(path):
    with pytest.raises(ValueError):
        adapter.validate({**INSPECT, "path": path})


@pytest.mark.parametrize(
    "extra",
    [
        {"repository": "--upload-pack=evil/repo"},
        {"repository": "https://github.com/o/r"},
        {"repository": "owner/repo.git"},
        {"branch": "main"},
        {"force": True},
        {"refspec": "HEAD:main"},
        {"config": "core.hooksPath=evil"},
        {"shell": "touch pwned"},
    ],
)
def test_arguments_cannot_supply_transport_execution_or_branch(extra):
    with pytest.raises(ValueError):
        adapter.validate({**INSPECT, **extra})


def test_stale_base_no_change_binary_symlink_and_parent_alias_are_denied(tmp_path, upstream):
    for i, args in enumerate(
        [
            create_args(upstream, expected_base="0" * 40),
            create_args(upstream, content="Original documentation\n"),
            create_args(upstream, path="alias"),
            create_args(upstream, path="alias/child"),
        ]
    ):
        result = run(tmp_path / f"private-{i}", args)
        assert "error" in result and not result["ambiguous"], result
    with pytest.raises(ValueError):
        adapter.validate(create_args(upstream, content="text\0binary"))
    with pytest.raises(UnicodeError):
        adapter.validate(create_args(upstream, content="\ud800"))
    assert not upstream.posts
    assert upstream.git("for-each-ref", "--format=%(refname)") == "refs/heads/main"


def test_new_file_preserves_entire_existing_tree(tmp_path, upstream):
    result = run(tmp_path / "private", create_args(upstream, path="docs/new.md"))
    assert "error" not in result, result
    assert upstream.git("show", result["branch"] + ":docs/new.md") == "Reviewed documentation: café 🌍"
    assert upstream.git("show", result["branch"] + ":README.md") == "Original documentation"


def test_atomic_create_only_lease_cannot_update_existing_ref(tmp_path, upstream, monkeypatch):
    monkeypatch.setattr(adapter.uuid, "uuid4", lambda: SimpleNamespace(hex="a" * 32))
    upstream.collision = True
    result = run(tmp_path / "private", create_args(upstream))
    assert "error" in result and result["stage"] == "push" and result["replay_forbidden"]
    assert upstream.git("rev-parse", "refs/heads/kars/reviewed/" + "a" * 32) == upstream.initial
    assert upstream.git("rev-parse", "refs/heads/main") == upstream.initial
    assert not upstream.posts


def test_generated_branch_equal_to_default_is_refused_before_mutation(tmp_path, upstream, monkeypatch):
    monkeypatch.setattr(adapter.uuid, "uuid4", lambda: SimpleNamespace(hex="a" * 32))
    upstream.default_branch = "kars/reviewed/" + "a" * 32
    upstream.git("update-ref", "refs/heads/" + upstream.default_branch, upstream.initial)
    result = run(tmp_path / "private", create_args(upstream))
    assert result["error"] == "Refusing default-branch mutation" and not result["ambiguous"]
    assert upstream.git("rev-parse", "refs/heads/" + upstream.default_branch) == upstream.initial
    assert not upstream.posts and not any("git-receive-pack" in path for _, path in upstream.calls)


@pytest.mark.parametrize("failure", ["drop", "invalid", "rejected"])
def test_ambiguous_post_is_retained_and_never_replayed(tmp_path, upstream, failure):
    upstream.drop = failure == "drop"
    upstream.invalid = failure == "invalid"
    upstream.post_status = 403 if failure == "rejected" else 201
    root = tmp_path / "private"
    result = run(root, create_args(upstream))
    assert result["ambiguous"] and result["replay_forbidden"] and "error" in result
    assert result["stage"] == "pr_create"
    assert json.loads(next(root.glob("publication-*.json")).read_text()) == result
    assert run(root, create_args(upstream))["replay_forbidden"]
    assert len(upstream.posts) == 1


def test_metadata_router_error_is_not_a_success_or_mutation(tmp_path, upstream):
    upstream.metadata_status = 403
    result = run(tmp_path / "private", INSPECT)
    assert result["error"] == "Router GitHub HTTP 403" and not result["ambiguous"]
    assert upstream.calls == [("GET", "/gh-api/repos/owner/repo")]


def test_private_workspace_rejects_symlinks_and_permissive_ownership(tmp_path):
    (tmp_path / "alias").symlink_to(tmp_path, target_is_directory=True)
    with pytest.raises(OSError):
        adapter.Engine(tmp_path / "alias" / "private")
    broad = tmp_path / "broad"
    broad.mkdir(mode=0o755)
    with pytest.raises(ValueError, match="private"):
        adapter.Engine(broad)


def test_host_environment_hooks_config_and_helpers_are_not_inherited(tmp_path, upstream, monkeypatch):
    poison = tmp_path / "poison.config"
    poison.write_text(
        "[core]\n hooksPath = /does-not-exist\n[credential]\n helper = !exit 99\n"
        '[url "file:///does-not-exist/"]\n insteadOf = http://\n'
    )
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(poison))
    monkeypatch.setenv("GIT_CONFIG_COUNT", "1")
    monkeypatch.setenv("GIT_CONFIG_KEY_0", "protocol.http.allow")
    monkeypatch.setenv("GIT_CONFIG_VALUE_0", "never")
    monkeypatch.setenv("GIT_EXEC_PATH", "/does-not-exist")
    monkeypatch.setenv("GIT_SSH_COMMAND", "exit 99")
    monkeypatch.setenv("HTTP_PROXY", "http://127.0.0.1:1")
    assert "error" not in run(tmp_path / "private", create_args(upstream))


@pytest.mark.parametrize(
    "expected,observed,accepted",
    [
        (1, 1, True),
        (42, 42, True),
        (42, 1, False),
        (42, 43, False),
    ],
)
def test_parent_lifetime_uses_prefork_identity(monkeypatch, expected, observed, accepted):
    calls = []
    monkeypatch.setattr(adapter.sys, "platform", "linux")
    monkeypatch.setattr(adapter.os, "getppid", lambda: observed)
    monkeypatch.setattr(
        adapter.ctypes, "CDLL", lambda *args, **kwargs: SimpleNamespace(prctl=lambda *args: calls.append(args) or 0)
    )
    if accepted:
        adapter.parent_death_signal(expected)
    else:
        with pytest.raises(RuntimeError, match="parent is no longer active"):
            adapter.parent_death_signal(expected)
    assert calls == [(1, adapter.signal.SIGTERM, 0, 0, 0)]


@pytest.mark.parametrize("expected", [None, True, 0, -1, "1"])
def test_parent_lifetime_requires_explicit_host_identity(expected):
    with pytest.raises(ValueError, match="host-owned parent process identity"):
        adapter.parent_death_signal(expected)


def test_parent_lifetime_registration_failure_still_fails_closed(monkeypatch):
    monkeypatch.setattr(adapter.sys, "platform", "linux")
    monkeypatch.setattr(adapter.ctypes, "CDLL", lambda *args, **kwargs: SimpleNamespace(prctl=lambda *args: -1))
    with pytest.raises(OSError, match="Cannot bind Git helper lifetime"):
        adapter.parent_death_signal(1)


def test_cancelled_wrapper_never_starts_process(monkeypatch):
    monkeypatch.setattr(adapter.subprocess, "Popen", lambda *a, **kw: pytest.fail("No subprocess after cancellation"))
    with pytest.raises(ValueError, match="no longer active"):
        adapter.invoke(["task", "run", "assignment"], INSPECT, lambda: True)


def test_interrupted_real_git_push_kills_process_group_and_retains_ambiguity(tmp_path, upstream, monkeypatch):
    engine = adapter.Engine(tmp_path / "private")
    groups = []

    class InterruptedSelector(adapter.selectors.DefaultSelector):
        def select(self, timeout=None):
            ready = super().select(timeout)
            if engine.stage == "push" and any("service=git-receive-pack" in path for _, path in upstream.calls):
                groups.append(engine.child.pid)
                raise RuntimeError("GitHub invocation cancelled")
            return ready

    monkeypatch.setattr(adapter.selectors, "DefaultSelector", InterruptedSelector)
    try:
        result = engine.run(create_args(upstream))
        assert "cancelled" in result["error"] and result["ambiguous"] and result["replay_forbidden"]
        assert engine.child is None and len(groups) == 1
        assert json.loads(next(engine.root.glob("publication-*.json")).read_text()) == result
        assert not upstream.posts
    finally:
        engine.close()
    deadline = adapter.time.monotonic() + 5
    while True:
        try:
            os.killpg(groups[0], 0)
        except ProcessLookupError:
            break
        assert adapter.time.monotonic() < deadline, "Cancelled Git transport descendants survived"
        adapter.time.sleep(0.05)


def test_wrapper_terminates_then_reaps_unresponsive_helper(monkeypatch):
    class Child:
        def __init__(self):
            self.terminated = self.killed = self.waited = False

        def communicate(self, request, timeout):
            assert json.loads(request)["arguments"] == INSPECT
            raise subprocess.TimeoutExpired("fixed-helper", timeout)

        def poll(self):
            return None

        def terminate(self):
            self.terminated = True

        def wait(self, timeout=None):
            if timeout is not None:
                raise subprocess.TimeoutExpired("fixed-helper", timeout)
            self.waited = True

        def kill(self):
            self.killed = True

    child = Child()

    def spawn(argv, **kwargs):
        assert argv[1:4] == ["-I", "-S", "-B"] and argv[-2] == str(Path(adapter.__file__).resolve())
        assert argv[-1] == str(os.getpid())
        assert kwargs["env"] == {} and "shell" not in kwargs
        return child

    monkeypatch.setattr(adapter.subprocess, "Popen", spawn)
    checks = iter([False, False, True])
    with pytest.raises(ValueError, match="cancelled.*never replay"):
        adapter.invoke(["task", "run", "assignment"], INSPECT, lambda: next(checks))
    assert child.terminated and child.killed and child.waited
