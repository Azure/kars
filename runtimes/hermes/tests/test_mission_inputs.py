# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json

import pytest

from kars_runtime_hermes.plugin.mission_native import FilesystemTools, Measurements
from kars_runtime_hermes.plugin import mission_protocol as wire


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


@pytest.mark.parametrize("count", [16, 28, wire.MAX_INPUT_FILES])
def test_bundle_preserves_all_contents_and_provenance_in_one_read(tmp_path, input_artifact, count):
    (tmp_path / "inputs").mkdir()
    inputs = [input_artifact(f"file-{n:03}.md", f"Complete {n}\r\n✓ 日本語 🌍") for n in range(count)]
    for item in inputs:
        (tmp_path / "inputs" / item["name"]).write_bytes(item["content"].encode())
    measured = Measurements("model", None, lambda _: None)
    tools = FilesystemTools(tmp_path, measured, input_artifacts=inputs)
    inputs[0]["source"]["runNonce"] = "caller-mutation"
    reply = json.loads(tools.execute("read", {"path": "inputs"}))
    records = json.loads(reply["content"])
    assert len(reply["content"].encode()) <= wire.MAX_INPUT
    assert len(records) == count
    assert records[0]["source"]["runNonce"] == "upstream-run"
    assert all(item["content"] == (tmp_path / "inputs" / item["name"]).read_bytes().decode()
               for item in records)
    assert tools.unread == set()
    assert measured.successful == 1
    assert "error" in json.loads(tools.execute("write", {"path": "inputs", "content": "Overwrite"}))
    assert measured.successful == 1


@pytest.mark.parametrize("sizes", [
    (31553, 26140, 10785),
    (6196, 14522, 5685, 3615, 45629, 12109, 9141, 9500, 15944, 8654, 8654, 10911, 6554, 6554,
     2264, 45629, 14522, 6196, 3615, 5685, 4201, 11018, 1031, 6672, 2080, 4430, 6387, 3899),
    (35000, 13205, 19474, 43332, 17511, 5728, 4869, 12000, 7200),
    (35000, 13205, 19474, 43332, 17511, 5728, 4869, 18352, 17947, 9524, 64000, 64000),
])
def test_research_bundle_above_64kib_preserves_all_bytes_and_custody(tmp_path, input_artifact, sizes):
    (tmp_path / "inputs").mkdir()
    inputs = [input_artifact(f"research-{i}.md", "x" * (size - 4) + "\n✓")
              for i, size in enumerate(sizes)]
    for item in inputs:
        (tmp_path / "inputs" / item["name"]).write_bytes(item["content"].encode())
    measured = Measurements("model", None, lambda _: None)
    tools = FilesystemTools(tmp_path, measured, input_artifacts=inputs)
    reply = json.loads(tools.execute("read", {"path": "inputs"}))
    assert 65536 < len(reply["content"].encode()) <= wire.MAX_INPUT
    assert json.loads(reply["content"]) == sorted(inputs, key=lambda item: item["name"])
    assert tools.unread == set()
    assert measured.successful == 1


def test_aggregate_count_does_not_expand_per_producer_exports(input_artifact):
    assert wire.MAX_INPUT_FILES == 8 * 16 + 8 + 16
    documents = [input_artifact(f"file-{n:03}.md") for n in range(wire.MAX_INPUT_FILES)]
    assert len(wire.inputs(documents)) == wire.MAX_INPUT_FILES
    with pytest.raises(ValueError):
        wire.inputs(documents + [input_artifact("overflow.md")])
    with pytest.raises(ValueError):
        wire.artifacts({f"file-{n}.md": "content" for n in range(17)})


def test_input_capacity_counts_utf8_and_json_escaping(input_artifact):
    prefix = [input_artifact(f"file-{i}.md", "x" * 65536) for i in range(5)]
    remaining = wire.MAX_INPUT - len(wire.encoded(prefix + [input_artifact("last.md", "x")])) + 1
    boundary = prefix + [input_artifact("last.md", "x" * remaining)]
    assert len(wire.encoded(wire.inputs(boundary))) == wire.MAX_INPUT
    assert not wire.message_fits({"inputArtifacts": boundary})
    with pytest.raises(ValueError):
        wire.inputs(prefix + [input_artifact("last.md", "x" * (remaining + 1))])
    assert wire.inputs([input_artifact("escaped.md", "\n" * 40000 + "✓")])
    with pytest.raises(ValueError):
        wire.inputs([input_artifact(f"escaped-{i}.md", "\n" * 40000 + "✓") for i in range(5)])
    with pytest.raises(ValueError):
        wire.inputs([input_artifact("oversize.md", "x" * 65537)])


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
