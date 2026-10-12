#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bind hoisted native plugin dependencies to the image's OpenClaw host."""
from pathlib import Path


def link_host(projects, host):
    host = host.resolve(strict=True)
    if not (host / "package.json").is_file():
        raise RuntimeError("OpenClaw host package is missing")
    roots = sorted(projects.iterdir())
    if not roots:
        raise RuntimeError("No installed OpenClaw plugin projects found")
    peers = []
    for project in roots:
        modules = project / "node_modules"
        if project.is_symlink() or not project.is_dir() or modules.is_symlink() or not modules.is_dir():
            raise RuntimeError("Invalid installed plugin project: " + str(project))
        peer = modules / "openclaw"
        if peer.exists() or peer.is_symlink():
            if not peer.is_symlink() or peer.resolve() != host:
                raise RuntimeError("Conflicting OpenClaw host peer: " + str(peer))
        else:
            peers.append(peer)
    for peer in peers:
        peer.symlink_to(host, target_is_directory=True)


if __name__ == "__main__":
    link_host(Path("/opt/openclaw-managed/npm/projects"),
              Path("/usr/local/lib/node_modules/openclaw"))
