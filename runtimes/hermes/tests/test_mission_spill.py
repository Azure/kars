# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline storage tests; actual Hermes dispatch is covered in test_mission_native."""

import hashlib
import json
from pathlib import Path
import stat

import pytest

from kars_runtime_hermes.plugin import mission_spill as spill


@pytest.fixture
def archive(tmp_path):
    store = spill.MissionSpill(tmp_path)
    try:
        yield store
    finally:
        store.close()


def decode(path):
    records = [json.loads(line) for line in Path(path).read_text(encoding="utf-8").splitlines()]
    return records[0], "".join(record["text"] for record in records[1:])


def test_lossless_long_single_line_unicode_and_controls(archive):
    content = json.dumps({"body": "evidence" * 20000}) + "\n" + "雪😀\x00\r\t\u2028" * 1000
    path = Path(archive.persist(content, "../../unsafe-native-id"))
    header, recovered = decode(path)
    assert path.parent == archive.root / spill.DIRECTORY
    assert header["format"] == "kars-tool-result-v1"
    assert header["characters"] == len(content)
    assert header["utf8Bytes"] == len(content.encode())
    assert recovered == content
    assert path.stat().st_size > 65536
    assert stat.S_IMODE(path.stat().st_mode) == 0o400
    assert max(map(len, path.read_bytes().split(b"\n"))) < 1600
    assert archive.read_arguments(path, {"path": str(path)})["limit"] == 32
    assert archive.read_arguments(path, {"offset": 40, "limit": 8}) == {"offset": 40, "limit": 8}


def test_repeated_native_ids_do_not_overwrite_evidence(archive):
    first = archive.persist("first complete result", "same-id.txt")
    second = archive.persist("second complete result", "same-id.txt")
    assert first != second
    assert decode(first)[1] == "first complete result"
    assert decode(second)[1] == "second complete result"


@pytest.mark.parametrize("args", [{"offset": 0}, {"offset": -1}, {"offset": True}, {"offset": "1"},
                                  {"limit": 0}, {"limit": 33}, {"limit": True}, {"limit": None}])
def test_read_pages_are_bounded(archive, args):
    path = Path(archive.persist("source", "id"))
    with pytest.raises(ValueError, match="1-based offset"):
        archive.read_arguments(path, args)


def test_unissued_files_and_other_assignments_are_not_readable(archive, tmp_path):
    unissued = tmp_path / "unissued.txt"
    unissued.write_text("not a tool result")
    with pytest.raises(ValueError, match="not issued"):
        archive.read_arguments(unissued, {})
    other = spill.MissionSpill(tmp_path / "other-assignment")
    with pytest.raises(ValueError, match="not issued"):
        other.read_arguments(Path(archive.persist("private result", "id")), {})


@pytest.mark.parametrize("tamper", ["replace", "same_size", "hardlink", "symlink"])
def test_tampered_files_are_not_readable(archive, tmp_path, tamper):
    path = Path(archive.persist("private result", "id"))
    if tamper == "replace":
        path.unlink()
        path.write_text("replacement")
    elif tamper == "same_size":
        path.chmod(0o600)
        data = path.read_bytes()
        path.write_bytes(data.replace(b"private", b"changed"))
    elif tamper == "hardlink":
        (tmp_path / "extra-link").hardlink_to(path)
    else:
        path.unlink()
        secret = tmp_path / "secret"
        secret.write_text("never exposed")
        path.symlink_to(secret)
    with pytest.raises((ValueError, OSError)):
        archive.read_arguments(path, {})


@pytest.mark.parametrize("preexisting", ["directory", "symlink"])
def test_writer_never_adopts_existing_spill_directory(archive, tmp_path, preexisting):
    path = tmp_path / spill.DIRECTORY
    if preexisting == "directory":
        path.mkdir()
    else:
        outside = tmp_path / "home"
        outside.mkdir()
        path.symlink_to(outside, target_is_directory=True)
    assert archive.persist("source", "id") is None
    assert not list(path.iterdir())


def test_storage_limits_keep_existing_evidence(archive, monkeypatch):
    first = Path(archive.persist("retained evidence", "first"))
    digest = hashlib.sha256(first.read_bytes()).hexdigest()
    monkeypatch.setattr(spill, "MAX_FILES", 1)
    assert archive.persist("over file count", "second") is None
    monkeypatch.setattr(spill, "MAX_FILES", 64)
    monkeypatch.setattr(spill, "MAX_STORAGE_BYTES", archive.bytes)
    assert archive.persist("over byte quota", "second") is None
    assert archive.persist("", "empty") is None
    assert hashlib.sha256(first.read_bytes()).hexdigest() == digest
    assert len(list(first.parent.iterdir())) == 1


def test_encoding_expansion_and_input_size_are_bounded(archive, monkeypatch):
    monkeypatch.setattr(spill, "MAX_FILE_BYTES", 1024)
    assert archive.persist("x" * 1025, "too-large") is None
    assert archive.persist("雪" * 500, "too-many-bytes") is None
    assert archive.persist("\x00" * 500, "encoding-expansion") is None
    assert not (archive.root / spill.DIRECTORY).exists()


def test_write_failure_keeps_native_fallback_available(archive, monkeypatch):
    def denied(*_args, **_kwargs):
        raise OSError("disk unavailable")

    monkeypatch.setattr(spill.os, "open", denied)
    assert archive.persist("result", "id") is None
    assert not archive.files and archive.bytes == 0
