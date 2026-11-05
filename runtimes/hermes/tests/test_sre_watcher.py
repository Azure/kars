# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import fcntl
import json
from pathlib import Path
import signal
import subprocess
import sys
from unittest.mock import MagicMock

import pytest

from kars_runtime_hermes.plugin import sre_watcher as watcher


@pytest.fixture
def configured(monkeypatch, tmp_path):
    monkeypatch.setenv("SRE_ENABLED", "true")
    monkeypatch.setenv("HERMES_HOME", str(tmp_path))
    monkeypatch.setenv("KUBERNETES_SERVICE_HOST", "127.0.0.1")
    monkeypatch.setenv("KUBERNETES_SERVICE_PORT", "9446")
    for name in (
        "TELEGRAM_BOT_TOKEN",
        "SLACK_BOT_TOKEN",
        "DISCORD_BOT_TOKEN",
        "SRE_WATCHER_NOTIFY_TARGET",
        "SRE_WATCHER_ENABLED",
        "SRE_HEALTH_INTERVAL_SECONDS",
        "SRE_HEALTH_TIMEOUT_SECONDS",
    ):
        monkeypatch.delenv(name, raising=False)
    (tmp_path / "config.yaml").write_text(
        "model:\n  default: gpt-5.4-mini\n  provider: azure-foundry\n"
        "  base_url: http://127.0.0.1:8443/v1\n"
        "plugins:\n  enabled: [kars]\n",
        encoding="utf-8",
    )
    plugin = tmp_path / "plugins/kars"
    plugin.mkdir(parents=True)
    (plugin / "__init__.py").write_text("# test-only installed entry point\n")
    kube = MagicMock()
    kube.get.side_effect = lambda path: {"ready": True} if path == "/readyz" else {"items": []}
    kube.post.side_effect = lambda path, json: {
        "persisted": True,
        "sweep_id": json["sweep_id"],
        "digest": "a" * 64,
        "accepted_at_ms": json["finished_at_ms"],
        "enforcement_changed": False,
    }
    monkeypatch.setattr(watcher.sre_kube, "client", lambda: kube)
    router = MagicMock()
    monkeypatch.setattr(watcher.router_client, "call", router)
    return tmp_path, kube, router


def test_channel_free_start_checks_authority_and_runs_once_before_sleep(configured, monkeypatch):
    home, kube, router = configured
    turns = []
    monkeypatch.setattr(watcher, "_run_hermes", lambda h, p, t: turns.append((h, p, t)) or {"turn_status": "finished"})

    def sleep(seconds):
        assert seconds == 900
        assert len(turns) == 1
        raise KeyboardInterrupt

    monkeypatch.setattr(watcher.time, "sleep", sleep)
    with pytest.raises(KeyboardInterrupt):
        watcher.run()
    assert turns[0][0] == home
    assert "sre_diagnose" in turns[0][1]
    assert "Never approve or execute" in turns[0][1]
    assert kube.get.call_args_list[0].args == ("/readyz",)
    router.assert_called_once_with("GET", "/readyz")
    record = json.loads((home / "sre-latest-report.json").read_text())
    assert record["notification"] == "not configured; local report retained"


@pytest.mark.parametrize("failure", ["unprojected", "authority", "budget", "inventory", "config", "plugin"])
def test_missing_authority_or_budget_never_starts_inference(configured, monkeypatch, failure):
    home, kube, router = configured
    if failure == "unprojected":
        monkeypatch.setenv("KUBERNETES_SERVICE_HOST", "10.96.0.1")
    elif failure == "authority":
        kube.get.return_value = {"ready": False}
        kube.get.side_effect = None
    elif failure == "budget":
        router.return_value.raise_for_status.side_effect = RuntimeError("finite contract unavailable")
    elif failure == "inventory":
        kube.get.side_effect = lambda path: {"ready": True} if path == "/readyz" else {}
    elif failure == "config":
        (home / "config.yaml").write_text("model: {}\n")
    else:
        (home / "plugins/kars/__init__.py").unlink()
    turn = MagicMock()
    monkeypatch.setattr(watcher, "_run_hermes", turn)
    record = watcher._sweep(home, 180)
    assert record["turn_status"] == "blocked"
    assert record["error"]
    turn.assert_not_called()
    if failure == "unprojected":
        kube.post.assert_not_called()
        assert record["publication_error"]
    else:
        kube.post.assert_called_once()
        assert kube.post.call_args.args == ("/reports/latest",)
        assert kube.post.call_args.kwargs["json"]["turn_status"] == "blocked"
        assert record["publication"]["persisted"] is True


def test_duplicate_supervisor_never_runs_or_waits(configured, monkeypatch):
    home, _, _ = configured
    sweep = MagicMock()
    monkeypatch.setattr(watcher, "_sweep", sweep)
    with (home / "sre-watcher.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        watcher.run()
    sweep.assert_not_called()


@pytest.mark.parametrize("model", [None, "", "   ", 54, ["gpt-5.4-mini"]])
def test_missing_or_invalid_configured_model_blocks_before_inference(configured, monkeypatch, model):
    home, kube, router = configured
    path = home / "config.yaml"
    config = watcher.yaml.safe_load(path.read_text())
    config["model"]["default"] = model
    path.write_text(watcher.yaml.safe_dump(config))
    turn = MagicMock()
    monkeypatch.setattr(watcher, "_run_hermes", turn)
    result = watcher._sweep(home, 180)
    assert result["turn_status"] == "blocked"
    assert "explicit installed Hermes model" in result["error"]
    turn.assert_not_called()
    router.assert_not_called()
    kube.get.assert_not_called()


def test_blocked_startup_rechecks_without_starting_inference(configured, monkeypatch):
    _, kube, _ = configured
    kube.get.side_effect = RuntimeError("authority still starting")
    turn = MagicMock()
    monkeypatch.setattr(watcher, "_run_hermes", turn)

    def sleep(seconds):
        assert seconds == 30
        raise KeyboardInterrupt

    monkeypatch.setattr(watcher.time, "sleep", sleep)
    with pytest.raises(KeyboardInterrupt):
        watcher.run()
    turn.assert_not_called()


@pytest.mark.parametrize("value", ["0", "59", "-10", "not-a-number", "86401"])
def test_invalid_cadence_fails_before_a_turn(configured, monkeypatch, value):
    monkeypatch.setenv("SRE_HEALTH_INTERVAL_SECONDS", value)
    with pytest.raises(ValueError):
        watcher.run()


def test_optional_channels_do_not_default_to_telegram(configured, monkeypatch):
    monkeypatch.setenv("SLACK_BOT_TOKEN", "test-only")
    assert watcher._notify_target() is None
    monkeypatch.setenv("SRE_WATCHER_NOTIFY_TARGET", "slack:review-channel")
    assert watcher._notify_target() == "slack:review-channel"
    monkeypatch.setenv("SRE_WATCHER_NOTIFY_TARGET", "telegram:123")
    with pytest.raises(ValueError, match="configured channel"):
        watcher._notify_target()


def test_notification_failure_does_not_discard_the_turn(configured, monkeypatch):
    home, _, _ = configured
    monkeypatch.setenv("SRE_WATCHER_NOTIFY_TARGET", "discord:no-credentials")
    monkeypatch.setattr(watcher, "_run_hermes", lambda *args: {"turn_status": "finished", "output": "Evidence"})
    record = watcher._sweep(home, 180)
    assert record["turn_status"] == "finished"
    assert record["notification_error"]
    assert json.loads((home / "sre-latest-report.json").read_text())["output"] == "Evidence"


@pytest.mark.parametrize("timeout", [False, True])
def test_native_turn_pins_router_tools_and_reaps_process_group(configured, monkeypatch, timeout):
    home, _, _ = configured
    monkeypatch.setenv("HERMES_INFERENCE_MODEL", "unrelated-ambient-model")
    original_popen = subprocess.Popen
    processes = []

    def popen(command, **options):
        assert command[:10] == [
            "hermes",
            "--accept-hooks",
            "--cli",
            "--provider",
            "azure-foundry",
            "--model",
            "gpt-5.4-mini",
            "--toolsets",
            "sre",
            "-z",
        ]
        assert options["env"]["AZURE_FOUNDRY_BASE_URL"] == "http://127.0.0.1:8443/v1"
        assert options["env"]["AZURE_FOUNDRY_API_KEY"] == "router-managed"
        assert options["stdin"] == subprocess.DEVNULL
        assert options["start_new_session"] is True
        assert options["stdout"] == subprocess.PIPE
        assert options["stderr"] == subprocess.STDOUT
        script = (
            "import time; print('partial diagnosis', flush=True); time.sleep(10)"
            if timeout
            else "print('actual subprocess output')"
        )
        process = original_popen([sys.executable, "-c", script], **options)
        processes.append(process)
        return process

    monkeypatch.setattr(watcher.subprocess, "Popen", popen)
    kill = MagicMock(wraps=watcher.os.killpg)
    monkeypatch.setattr(watcher.os, "killpg", kill)
    result = watcher._run_hermes(home, "health", 0.1 if timeout else 5)
    assert result["turn_status"] == ("timeout" if timeout else "finished")
    kill.assert_called_once_with(processes[0].pid, signal.SIGKILL)
    assert processes[0].poll() is not None
    assert processes[0].stdout.closed
    if timeout:
        assert result["output"] == "partial diagnosis\n"
    else:
        assert result["output"] == "actual subprocess output\n"
        assert result["output_truncated"] is False


@pytest.mark.parametrize("extra_bytes", [0, 1, 65536])
def test_combined_output_is_bounded_and_overflow_fails(configured, monkeypatch, extra_bytes):
    home, _, _ = configured
    original_popen = subprocess.Popen
    processes = []
    limit = watcher.MAX_TURN_LOG_BYTES

    def popen(_command, **options):
        script = f"import os,time;os.write(1,b'a'*{limit // 2});os.write(2,b'b'*{limit // 2 + extra_bytes});" + (
            "time.sleep(10)" if extra_bytes else ""
        )
        process = original_popen([sys.executable, "-c", script], **options)
        processes.append(process)
        return process

    monkeypatch.setattr(watcher.subprocess, "Popen", popen)
    kill = MagicMock(wraps=watcher.os.killpg)
    monkeypatch.setattr(watcher.os, "killpg", kill)
    result = watcher._run_hermes(home, "health", 5)
    data = (home / "sre-latest-turn.log").read_bytes()
    assert len(data) == limit
    assert data == b"a" * (limit // 2) + b"b" * (limit // 2)
    assert result["turn_status"] == ("failed" if extra_bytes else "finished")
    assert result["output"].startswith("a" * 100)
    assert result["output"].endswith("b" * 100)
    assert len(result["output"].encode()) <= 65536
    if extra_bytes:
        assert "log limit" in result["error"]
        assert result["output_truncated"] is True
        assert processes[0].returncode == -signal.SIGKILL
    kill.assert_called_once_with(processes[0].pid, signal.SIGKILL)
    assert processes[0].poll() is not None
    assert processes[0].stdout.closed


def test_entrypoint_sre_start_is_not_channel_gated():
    source = (Path(__file__).resolve().parents[3] / "sandbox-images/hermes/entrypoint.sh").read_text()
    block = source.split("# SRE sweeps also run without a human channel.", 1)[1].split("\n  fi", 1)[0]
    assert "WANT_GATEWAY" not in block
    assert "python3 -m kars_runtime_hermes.plugin.sre_watcher &" in block
