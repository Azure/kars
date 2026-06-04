# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Identity store — persistent Ed25519 signing + X25519 ratchet keys.

The identity DID is derived deterministically from the Ed25519 public
key so it is stable across restarts as long as the key file is
preserved (the controller mounts ``identity_path`` to an emptyDir by
default; operators wanting cross-pod-restart identity should mount a
PersistentVolume).
"""

from __future__ import annotations

import base64
import json
import logging
import os
from dataclasses import dataclass
from pathlib import Path

from nacl import signing
from nacl.bindings import crypto_scalarmult_base
from nacl.public import PrivateKey as X25519PrivateKey

logger = logging.getLogger("kars_agt_mesh.identity")


def _b64url(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode("ascii")


def _b64url_decode(s: str) -> bytes:
    pad = "=" * (-len(s) % 4)
    return base64.urlsafe_b64decode(s + pad)


@dataclass(frozen=True)
class Identity:
    """A persistent agent identity. Use :meth:`IdentityStore.load_or_create`
    to construct — never instantiate directly."""

    ed25519_seed: bytes  # 32 bytes
    x25519_secret: bytes  # 32 bytes

    @property
    def signing_key(self) -> signing.SigningKey:
        """Ed25519 SigningKey, used for DID derivation + registry
        proof-of-possession + KNOCK signing."""
        return signing.SigningKey(self.ed25519_seed)

    @property
    def verify_key_bytes(self) -> bytes:
        """32-byte Ed25519 public key, the basis of the DID."""
        return self.signing_key.verify_key.encode()

    @property
    def x25519_private(self) -> X25519PrivateKey:
        """X25519 PrivateKey for ratchet/X3DH operations."""
        return X25519PrivateKey(self.x25519_secret)

    @property
    def x25519_public_bytes(self) -> bytes:
        """32-byte X25519 public key (sodium scalarmult of secret · base)."""
        return crypto_scalarmult_base(self.x25519_secret)

    @property
    def did(self) -> str:
        """``did:mesh:<sha256(ed25519_pub)[:32]>`` — matches the
        canonical DID derivation in the AGT Python registry
        (registry/app.py: ``did = f"did:mesh:{key_hash}"`` where
        ``key_hash = hashlib.sha256(public_key).hexdigest()[:32]``)."""
        import hashlib

        return f"did:mesh:{hashlib.sha256(self.verify_key_bytes).hexdigest()[:32]}"


class IdentityStore:
    """Loads or creates a persistent :class:`Identity` from a JSON file
    on disk.

    Wire shape of the file (kept stable across versions so a TS SDK
    can read a Python-written file and vice versa — both layouts share
    the same field names as ``@microsoft/agent-governance-sdk``'s
    ``IdentityStore``):

    .. code-block:: json

        {
          "version": 1,
          "ed25519_seed": "<base64url, 32 bytes>",
          "x25519_secret": "<base64url, 32 bytes>",
          "did": "<canonical DID for cross-check>"
        }
    """

    SCHEMA_VERSION = 1

    @classmethod
    def load_or_create(cls, path: Path) -> Identity:
        """Load an existing identity from ``path`` or create+persist a
        fresh one if the file doesn't exist.

        Parent directory is created with mode 0700 if missing. The
        file itself is written with mode 0600 (owner-only) so a
        compromised sibling container can't lift the keys.
        """
        path = Path(path)
        if path.exists():
            return cls._load(path)
        return cls._create_and_persist(path)

    @classmethod
    def _load(cls, path: Path) -> Identity:
        try:
            raw = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError) as exc:
            raise RuntimeError(
                f"Failed to load identity from {path}: {exc}. "
                "Delete the file to regenerate; existing peer sessions "
                "will be invalidated."
            ) from exc

        if raw.get("version") != cls.SCHEMA_VERSION:
            raise RuntimeError(
                f"Identity file {path} has version {raw.get('version')}, "
                f"expected {cls.SCHEMA_VERSION}. Migration not yet "
                "implemented; delete the file to regenerate."
            )

        try:
            ed_seed = _b64url_decode(raw["ed25519_seed"])
            x_secret = _b64url_decode(raw["x25519_secret"])
        except (KeyError, ValueError) as exc:
            raise RuntimeError(
                f"Identity file {path} missing or malformed key material: {exc}"
            ) from exc

        if len(ed_seed) != 32 or len(x_secret) != 32:
            raise RuntimeError(
                f"Identity file {path}: Ed25519 seed = {len(ed_seed)} bytes, "
                f"X25519 secret = {len(x_secret)} bytes (both must be 32)"
            )

        identity = Identity(ed25519_seed=ed_seed, x25519_secret=x_secret)
        # Sanity check: stored DID matches derived DID. Prevents
        # silent identity drift when an operator hand-edits the file.
        stored_did = raw.get("did")
        if stored_did and stored_did != identity.did:
            logger.warning(
                "Identity file %s stored did=%s but ed25519_seed derives did=%s; "
                "using derived value",
                path,
                stored_did,
                identity.did,
            )
        return identity

    @classmethod
    def _create_and_persist(cls, path: Path) -> Identity:
        # Fresh entropy from libsodium's CSPRNG.
        signing_key = signing.SigningKey.generate()
        x_key = X25519PrivateKey.generate()

        identity = Identity(
            ed25519_seed=bytes(signing_key),
            x25519_secret=bytes(x_key),
        )

        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        body = {
            "version": cls.SCHEMA_VERSION,
            "ed25519_seed": _b64url(identity.ed25519_seed),
            "x25519_secret": _b64url(identity.x25519_secret),
            "did": identity.did,
        }
        # Write then atomic-replace so a crash mid-write doesn't
        # leave a partially-initialized file that the next boot
        # mis-loads.
        tmp = path.with_suffix(path.suffix + ".tmp")
        tmp.write_text(json.dumps(body, indent=2))
        os.chmod(tmp, 0o600)
        os.replace(tmp, path)
        logger.info(
            "Created new identity at %s with did=%s",
            path,
            identity.did,
        )
        return identity
