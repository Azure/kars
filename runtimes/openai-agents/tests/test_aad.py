# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import time
from dataclasses import dataclass

import pytest

from azureclaw_runtime_openai_agents import aad


@dataclass
class _FakeAccessToken:
    token: str
    expires_on: int


class _FakeCredential:
    def __init__(self, tokens):
        self._tokens = list(tokens)
        self.calls = []

    def get_token(self, *scopes):
        self.calls.append(scopes)
        return self._tokens.pop(0)


def test_get_token_returns_token():
    cred = _FakeCredential([_FakeAccessToken("abc", int(time.time()) + 3600)])
    broker = aad.TokenBroker(credential=cred)
    assert broker.get_token() == "abc"
    assert cred.calls == [(aad.DEFAULT_SCOPE,)]


def test_get_token_caches_within_skew():
    expires = int(time.time()) + 3600
    cred = _FakeCredential([_FakeAccessToken("first", expires)])
    broker = aad.TokenBroker(credential=cred)
    assert broker.get_token() == "first"
    # Second call must hit the cache, not the credential.
    assert broker.get_token() == "first"
    assert len(cred.calls) == 1


def test_get_token_refreshes_when_within_skew():
    near_expiry = int(time.time()) + 60  # within 5-min skew
    cred = _FakeCredential(
        [
            _FakeAccessToken("first", near_expiry),
            _FakeAccessToken("second", int(time.time()) + 7200),
        ]
    )
    broker = aad.TokenBroker(credential=cred, skew_seconds=300)
    assert broker.get_token() == "first"
    assert broker.get_token() == "second"
    assert len(cred.calls) == 2


def test_get_token_separate_cache_per_scope():
    cred = _FakeCredential(
        [
            _FakeAccessToken("a", int(time.time()) + 3600),
            _FakeAccessToken("b", int(time.time()) + 3600),
        ]
    )
    broker = aad.TokenBroker(credential=cred)
    assert broker.get_token("scope-a") == "a"
    assert broker.get_token("scope-b") == "b"
    # Cached on subsequent calls.
    assert broker.get_token("scope-a") == "a"
    assert len(cred.calls) == 2


def test_invalidate_clears_cache():
    cred = _FakeCredential(
        [
            _FakeAccessToken("first", int(time.time()) + 3600),
            _FakeAccessToken("second", int(time.time()) + 3600),
        ]
    )
    broker = aad.TokenBroker(credential=cred)
    assert broker.get_token() == "first"
    broker.invalidate()
    assert broker.get_token() == "second"


def test_module_level_get_token_uses_default_broker(monkeypatch):
    fake = _FakeCredential([_FakeAccessToken("env-token", int(time.time()) + 3600)])
    # Patch the lazy credential factory to inject our fake.
    monkeypatch.setattr(
        aad.TokenBroker, "_build_default_credential", lambda self: fake
    )
    aad.reset_default_broker()
    assert aad.get_token() == "env-token"
    # Cached on the module singleton.
    assert aad.get_token() == "env-token"
    assert len(fake.calls) == 1


def test_default_credential_lazy_import_does_not_fail_without_call():
    # Constructing a broker without injecting a credential and never
    # calling get_token must not require azure-identity.
    broker = aad.TokenBroker()
    assert broker is not None  # smoke
