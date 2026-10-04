# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Descriptor-relative filesystem operations for the OpenClaw task loop."""

import base64
import json
import os
import posixpath
import secrets
import stat
import sys
from contextlib import contextmanager

MAX_BYTES = 16 * 1024 * 1024
MAX_REQUEST_BYTES = 24 * 1024 * 1024
DIRECTORY_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW


@contextmanager
def parent_directory(path, create):
    if not isinstance(path, str) or not path.startswith("/") or "\0" in path:
        raise ValueError("path must be absolute")
    path = posixpath.normpath(path)
    root = next((root for root in ("/sandbox", "/tmp") if path.startswith(root + "/")), None)
    if root is None or len(path.encode("utf-8")) > 4096:
        raise ValueError("path must resolve under /sandbox/ or /tmp/ and fit 4096 bytes")
    components = path[len(root) + 1:].split("/")
    # Only the administrator-owned root may be an alias (macOS /tmp). Every
    # untrusted component is opened relative to an already-held directory FD.
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
    try:
        for component in components[:-1]:
            try:
                child = os.open(component, DIRECTORY_FLAGS, dir_fd=fd)
            except FileNotFoundError:
                if not create:
                    raise
                try:
                    os.mkdir(component, mode=0o700, dir_fd=fd)
                except FileExistsError:
                    pass
                child = os.open(component, DIRECTORY_FLAGS, dir_fd=fd)
            os.close(fd)
            fd = child
        yield fd, components[-1], path
    finally:
        os.close(fd)


def read_file(path, limit):
    if type(limit) is not int or not 1 <= limit <= MAX_BYTES:
        raise ValueError("max_bytes must be a whole number between 1 and 16777216")
    with parent_directory(path, False) as (parent, name, resolved):
        # NONBLOCK prevents a FIFO from blocking before fstat can reject it.
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode):
                raise ValueError("not a regular file")
            chunks = []
            remaining = min(info.st_size, limit)
            while remaining:
                chunk = os.read(fd, min(remaining, 65536))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
            content = b"".join(chunks)
            return {"path": resolved, "bytes": info.st_size,
                    "truncated": len(content) < info.st_size,
                    "returned_bytes": len(content),
                    "base64": base64.b64encode(content).decode("ascii")}
        finally:
            os.close(fd)


def write_file(path, encoded):
    if not isinstance(encoded, str):
        raise ValueError("content must be base64-encoded bytes")
    content = base64.b64decode(encoded, validate=True)
    if len(content) > MAX_BYTES:
        raise ValueError("content exceeds 16777216 bytes")
    with parent_directory(path, True) as (parent, name, resolved):
        try:
            info = os.stat(name, dir_fd=parent, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            if not stat.S_ISREG(info.st_mode):
                raise ValueError("refusing to replace a symlink or non-regular file")
        # Never truncate an existing inode: it could be hard-linked elsewhere.
        # Replacing this directory entry also cannot follow a raced-in symlink.
        temporary = ".kars-write-" + secrets.token_hex(24)
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=parent)
        try:
            try:
                remaining = memoryview(content)
                while remaining:
                    written = os.write(fd, remaining)
                    if written <= 0:
                        raise OSError("file write made no progress")
                    remaining = remaining[written:]
            finally:
                os.close(fd)
            os.replace(temporary, name, src_dir_fd=parent, dst_dir_fd=parent)
        finally:
            try:
                os.unlink(temporary, dir_fd=parent)
            except FileNotFoundError:
                pass
        return {"path": resolved, "bytes": len(content)}


def execute(request):
    if request.get("operation") == "read":
        return read_file(request["path"], request["max_bytes"])
    if request.get("operation") == "write":
        return write_file(request["path"], request["base64"])
    raise ValueError("unsupported filesystem operation")


if __name__ == "__main__":
    try:
        raw = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
        if len(raw) > MAX_REQUEST_BYTES:
            raise ValueError("filesystem request exceeds its byte limit")
        result = execute(json.loads(raw))
        print(json.dumps({"result": result}, separators=(",", ":")))
    except Exception as error:
        print(json.dumps({"error": str(error)[:1024]}, separators=(",", ":")))
        sys.exit(1)
