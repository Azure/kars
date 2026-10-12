# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import hashlib
import json
from pathlib import Path

import pytest

from kars_runtime_hermes.plugin.mission_skills import SkillFiles
from kars_runtime_hermes.plugin.mission_native import FilesystemTools, Measurements

FIXTURE = json.loads((Path(__file__).resolve().parents[3] / "tests/fixtures/skill-package.json").read_text())
DIGEST = "sha256:ff733becaecde141e87b4b8223ca99091bb181295398a1ddace419976dd16615"


def catalog():
    return {"schema": 1, "sandboxUid": "sandbox-uid", "skills": [{
        "name": "checklist", "uid": "skill-uid", "generation": 2, "versionDigest": "sha256:" + "a" * 32,
        "summary": "Review local evidence", "recipe": None, "packageDigest": DIGEST, "files": dict(FIXTURE),
    }]}


def load(value):
    raw = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
    return SkillFiles(raw, "sha256:" + hashlib.sha256(raw).hexdigest())


def test_shared_canonical_unicode_numeric_names_and_empty_file():
    skills = load(catalog())
    for path, content in FIXTURE.items():
        assert skills.read("skills/checklist/" + path) == content
    assert "SKILL.md" in skills.instructions()
    assert "does not grant shell" in skills.instructions()


def test_no_ambient_discovery_without_controller_binding(monkeypatch):
    monkeypatch.delenv("KARS_SKILL_CATALOG_DIGEST", raising=False)
    assert SkillFiles().files == {}


@pytest.mark.parametrize("path", ["../secret", "/absolute", "a//b", "a/./b", "a__b", "a/../b", "x/" * 85 + "z"])
def test_invalid_source_paths(path):
    value = catalog()
    value["skills"][0]["files"][path] = "content"
    with pytest.raises(ValueError, match="Invalid approved skill file"):
        load(value)


@pytest.mark.parametrize("change", ["parent", "missing", "duplicate", "content", "identity"])
def test_manifest_identity_and_content_failures(change):
    value = catalog()
    skill = value["skills"][0]
    if change == "parent":
        skill["files"]["references"] = "not a directory"
    elif change == "missing":
        del skill["files"]["SKILL.md"]
    elif change == "duplicate":
        value["skills"].append(dict(skill))
    elif change == "content":
        skill["files"]["SKILL.md"] += "Changed"
    else:
        skill["generation"] = True
    with pytest.raises(ValueError):
        load(value)


def test_controller_binding_and_catalog_size():
    with pytest.raises(ValueError, match="controller-bound"):
        SkillFiles(b"{}", "sha256:" + "0" * 64)
    raw = b" " * (1024 * 1024 + 1)
    with pytest.raises(ValueError, match="controller-bound"):
        SkillFiles(raw, "sha256:" + hashlib.sha256(raw).hexdigest())


def test_virtual_tool_reads_measured_read_only_and_no_disk_fallback(tmp_path):
    events = []
    measurements = Measurements("model", None, events.append)
    tools = FilesystemTools(tmp_path, measurements, load(catalog()))
    path = "skills/checklist/SKILL.md"
    assert json.loads(tools.execute("read", {"path": path})) == {"content": FIXTURE["SKILL.md"]}
    assert measurements.successful == 1 and len(events) == 1
    assert "error" in json.loads(tools.execute("write", {"path": path, "content": "replace"}))
    for path in ["skills/other/SKILL.md", "skills/checklist/../SKILL.md", "skills/checklist/missing"]:
        assert "error" in json.loads(tools.execute("read", {"path": path}))
    assert measurements.successful == 1
    assert list(tmp_path.iterdir()) == []


def test_valid_long_virtual_path_preserves_ordinary_filesystem_limit(tmp_path):
    value = catalog()
    name = "a" * 253
    files = value["skills"][0]["files"]
    files[name] = "long-path content"
    encoded = {path.replace("/", "__"): content for path, content in files.items()}
    raw = json.dumps(encoded, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()
    value["skills"][0]["packageDigest"] = "sha256:" + hashlib.sha256(raw).hexdigest()
    tools = FilesystemTools(tmp_path, Measurements("model", None, lambda _: None), load(value))
    assert json.loads(tools.execute("read", {"path": "skills/checklist/" + name})) == {"content": files[name]}
    assert "error" in json.loads(tools.execute("read", {"path": "inputs/" + name}))


def test_recipe_only_has_no_virtual_files():
    value = catalog()
    value["skills"][0].update(files={}, packageDigest=None, recipe="Review only the provided task.")
    skills = load(value)
    assert not skills.files
    assert "Review only the provided task." in skills.instructions()
