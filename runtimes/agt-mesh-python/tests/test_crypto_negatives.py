from __future__ import annotations

from agentmesh.encryption.channel import SecureChannel
from agentmesh.encryption.ratchet import EncryptedMessage
from agentmesh.encryption.x3dh import X3DHKeyManager
from cryptography.exceptions import InvalidTag
from nacl.bindings import crypto_sign_keypair
import pytest


def _key_manager() -> X3DHKeyManager:
    public_key, private_key = crypto_sign_keypair()
    manager = X3DHKeyManager.from_ed25519_keys(private_key, public_key)
    manager.generate_signed_pre_key()
    manager.generate_one_time_pre_keys(5)
    return manager


def _channel_pair() -> tuple[SecureChannel, SecureChannel]:
    sender_keys = _key_manager()
    receiver_keys = _key_manager()
    sender, establishment = SecureChannel.create_sender(
        sender_keys,
        receiver_keys.get_public_bundle(),
        b"did:mesh:sender|did:mesh:receiver",
    )
    receiver = SecureChannel.create_receiver(
        receiver_keys,
        establishment,
        b"did:mesh:sender|did:mesh:receiver",
    )
    return sender, receiver


def test_replay_is_rejected_without_plaintext_on_wire() -> None:
    sender, receiver = _channel_pair()
    plaintext = b"HIDDEN_CROSS_RUNTIME_NONCE_8b134"
    encrypted = sender.send(plaintext)

    assert plaintext not in encrypted.ciphertext
    assert receiver.receive(encrypted) == plaintext
    with pytest.raises(InvalidTag):
        receiver.receive(encrypted)


def test_tampered_ciphertext_is_rejected() -> None:
    sender, receiver = _channel_pair()
    encrypted = sender.send(b"integrity-bound payload")
    tampered = bytearray(encrypted.ciphertext)
    tampered[-1] ^= 0x01

    with pytest.raises(InvalidTag):
        receiver.receive(
            EncryptedMessage(
                header=encrypted.header,
                ciphertext=bytes(tampered),
            )
        )


def test_wrong_recipient_cannot_decrypt() -> None:
    sender_keys = _key_manager()
    intended_recipient = _key_manager()
    wrong_recipient = _key_manager()
    sender, establishment = SecureChannel.create_sender(
        sender_keys,
        intended_recipient.get_public_bundle(),
        b"did:mesh:sender|did:mesh:intended",
    )
    attacker_channel = SecureChannel.create_receiver(
        wrong_recipient,
        establishment,
        b"did:mesh:sender|did:mesh:intended",
    )

    with pytest.raises(InvalidTag):
        attacker_channel.receive(sender.send(b"recipient-bound payload"))
