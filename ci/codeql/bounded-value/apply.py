#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Install the proven sink-local extension into an isolated, pinned CodeQL tree."""

import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parent
UPSTREAM_FILE = 'rust/ql/lib/codeql/rust/security/UnsafeDeserializationExtensions.qll'
CONTROL_DIRECTORY = 'rust/ql/test/query-tests/security/CWE-502-bounded-value-controls'
CONTROL_FILES = {
    'BoundedJsonValue.qll', 'BoundedValueControl.ql', 'BoundedValueControl.expected',
    'IntegratedFlowControl.ql', 'IntegratedFlowControl.expected', 'main.rs', 'options.yml',
}


def sha256(content):
    return hashlib.sha256(content).hexdigest()


def apply(source, evidence, commit):
    manifest = json.loads((ROOT / 'manifest.json').read_text())
    if commit != manifest['upstreamCommit'] or manifest['upstreamFile'] != UPSTREAM_FILE:
        raise SystemExit('bounded Value extension requires the validated upstream revision')
    source = source.resolve(strict=True)
    extension = source / UPSTREAM_FILE
    if extension.resolve(strict=True) != extension:
        raise SystemExit('bounded Value extension source must not be a symlink')
    original = extension.read_bytes()
    if sha256(original) != manifest['upstreamSha256']:
        raise SystemExit('bounded Value upstream source hash mismatch')

    helper = extension.with_name('BoundedJsonValue.qll')
    controls = source / CONTROL_DIRECTORY
    if helper.exists() or helper.is_symlink() or controls.exists() or controls.is_symlink():
        raise SystemExit('bounded Value extension or controls already exist')
    if controls.parent.resolve(strict=True) != controls.parent:
        raise SystemExit('bounded Value control directory must not be a symlink')
    payload = ROOT / 'controls'
    if set(manifest['controls']) != CONTROL_FILES or {p.name for p in payload.iterdir()} != CONTROL_FILES:
        raise SystemExit('bounded Value control file set mismatch')
    contents = {name: (payload / name).read_bytes() for name in CONTROL_FILES}
    for name, content in contents.items():
        if sha256(content) != manifest['controls'][name]:
            raise SystemExit(f'bounded Value control hash mismatch: {name}')

    # These are exactly the two source edits qualified by ch2b, not a global barrier.
    old = 'ModelsAsDataSink() { sinkNode(this, "unsafe-deserialization") }'
    new = ('ModelsAsDataSink() { sinkNode(this, "unsafe-deserialization") '
           'and not boundedStandardValueModeledSink(this) }')
    text = original.decode('utf-8')
    if text.count('import rust\n') != 1 or text.count(old) != 1:
        raise SystemExit('bounded Value upstream patch anchors mismatch')
    patched = text.replace(
        'import rust\n', 'import rust\nprivate import codeql.rust.security.BoundedJsonValue\n', 1,
    ).replace(old, new).encode('utf-8')
    if sha256(patched) != manifest['patchedSha256']:
        raise SystemExit('bounded Value patched source hash mismatch')

    controls.mkdir()
    for name, content in contents.items():
        (controls / name).write_bytes(content)
    helper.write_bytes(contents['BoundedJsonValue.qll'])
    extension.write_bytes(patched)
    evidence.mkdir(parents=True, exist_ok=True)
    (evidence / 'bounded-value-extension.json').write_text(json.dumps({
        **manifest,
        'source': str(source),
        'installedLibrary': str(helper),
        'installedControls': str(controls),
        'status': 'installed; behavioral controls must still pass',
    }, indent=2) + '\n')


if __name__ == '__main__':
    if len(sys.argv) != 4:
        raise SystemExit('usage: apply.py SOURCE EVIDENCE UPSTREAM_COMMIT')
    apply(Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3])
