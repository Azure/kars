# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json

import pytest

from kars_runtime_hermes.plugin.mission_native import FilesystemTools, Measurements


def test_complete_input_reads_precede_exports(tmp_path, input_artifact):
    (tmp_path / "inputs").mkdir()
    (tmp_path / "exports").mkdir()
    (tmp_path / "inputs" / "first.md").write_text("First complete handoff", encoding="utf-8")
    (tmp_path / "inputs" / "second.md").write_text("Second complete handoff", encoding="utf-8")
    measured = Measurements("model", None, lambda _: None)
    inputs = [input_artifact("first.md", "First complete handoff"),
              input_artifact("second.md", "Second complete handoff")]
    tools = FilesystemTools(tmp_path, measured, input_artifacts=inputs)
    write = {"path": "exports/report.md", "content": "Synthesis"}
    assert "Read all immutable inputs" in json.loads(tools.execute("write", write))["error"]
    assert not (tmp_path / "exports" / "report.md").exists()
    assert json.loads(tools.execute("read", {"path": "inputs/first.md"}))["content"] == "First complete handoff"
    assert tools.unread == {"inputs/second.md"}
    assert "error" in json.loads(tools.execute("write", write))
    assert json.loads(tools.execute("read", {"path": "inputs/second.md"}))["content"] == "Second complete handoff"
    assert tools.unread == set()
    assert json.loads(tools.execute("write", write)) == {"written": "exports/report.md"}
    assert tools.written == {"report.md"}
    assert measured.successful == 3
    assert FilesystemTools(tmp_path, measured, input_artifacts=inputs[:1]).unread == {"inputs/first.md"}


@pytest.mark.parametrize("failure", ["missing", "oversize", "invalid-utf8", "symlink"])
def test_failed_reads_never_satisfy_input_consumption(tmp_path, failure, input_artifact):
    (tmp_path / "inputs").mkdir()
    (tmp_path / "exports").mkdir()
    path = tmp_path / "inputs" / "facts.md"
    if failure == "oversize":
        path.write_bytes(b"a" * 65537)
    elif failure == "invalid-utf8":
        path.write_bytes(b"\xff")
    elif failure == "symlink":
        other = tmp_path / "other"
        other.write_text("Must not read")
        path.symlink_to(other)
    measured = Measurements("model", None, lambda _: None)
    tools = FilesystemTools(tmp_path, measured, input_artifacts=[input_artifact("facts.md")])
    assert "error" in json.loads(tools.execute("read", {"path": "inputs/facts.md"}))
    assert tools.unread == {"inputs/facts.md"}
    assert measured.successful == 0


def test_bundle_preserves_all_contents_and_provenance_in_one_read(tmp_path, input_artifact):
    (tmp_path / "inputs").mkdir()
    inputs = [input_artifact(f"file-{n:02}.md", f"Complete {n}\r\n✓ 日本語 🌍") for n in range(16)]
    for item in inputs:
        (tmp_path / "inputs" / item["name"]).write_bytes(item["content"].encode())
    measured = Measurements("model", None, lambda _: None)
    tools = FilesystemTools(tmp_path, measured, input_artifacts=inputs)
    inputs[0]["source"]["runNonce"] = "caller-mutation"
    reply = json.loads(tools.execute("read", {"path": "inputs"}))
    records = json.loads(reply["content"])
    assert len(reply["content"].encode()) <= 65536
    assert len(records) == 16
    assert records[0]["source"]["runNonce"] == "upstream-run"
    assert all(item["content"] == (tmp_path / "inputs" / item["name"]).read_bytes().decode()
               for item in records)
    assert tools.unread == set()
    assert measured.successful == 1
    assert "error" in json.loads(tools.execute("write", {"path": "inputs", "content": "Overwrite"}))
    assert measured.successful == 1


@pytest.mark.parametrize("failure", ["missing", "modified", "oversize", "invalid-utf8", "symlink", "parent-symlink"])
def test_bundle_failure_never_partially_consumes_inputs(tmp_path, input_artifact, failure):
    (tmp_path / "inputs").mkdir()
    inputs = [input_artifact("first.md"), input_artifact("second.md")]
    (tmp_path / "inputs" / "first.md").write_text(inputs[0]["content"])
    path = tmp_path / "inputs" / "second.md"
    if failure == "modified":
        path.write_text("Substituted bytes")
    elif failure == "oversize":
        path.write_bytes(b"a" * 65537)
    elif failure == "invalid-utf8":
        path.write_bytes(b"\xff")
    elif failure == "symlink":
        path.symlink_to(tmp_path / "inputs" / "first.md")
    elif failure == "parent-symlink":
        (tmp_path / "inputs").rename(tmp_path / "outside")
        (tmp_path / "inputs").symlink_to(tmp_path / "outside", target_is_directory=True)
    measured = Measurements("model", None, lambda _: None)
    tools = FilesystemTools(tmp_path, measured, input_artifacts=inputs)
    assert "error" in json.loads(tools.execute("read", {"path": "inputs"}))
    assert tools.unread == {"inputs/first.md", "inputs/second.md"}
    assert measured.successful == 0


@pytest.mark.parametrize("path", ["inputs/", "./inputs", "inputs/..", "../inputs", "inputs//"])
def test_bundle_aliases_are_not_accepted(tmp_path, input_artifact, path):
    measured = Measurements("model", None, lambda _: None)
    tools = FilesystemTools(tmp_path, measured, input_artifacts=[input_artifact("facts.md")])
    assert "error" in json.loads(tools.execute("read", {"path": path}))
    assert tools.unread == {"inputs/facts.md"}
    assert measured.successful == 0
