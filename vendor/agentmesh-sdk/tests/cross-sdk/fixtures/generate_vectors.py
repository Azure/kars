#!/usr/bin/env python3
"""
Generate test vectors for cross-SDK compatibility testing.
Run this script to generate values for test-vectors.ts.
"""

import base64
import json
import hmac
from hashlib import sha256
from nacl.public import PrivateKey, PublicKey
from nacl.signing import SigningKey, VerifyKey
from nacl.bindings import crypto_scalarmult
from nacl.secret import SecretBox
from nacl.utils import random


def b64(data: bytes) -> str:
    """Encode bytes to base64 string."""
    return base64.b64encode(data).decode('utf-8')


def derive_amid(signing_public_key: bytes) -> str:
    """Derive AMID from signing public key: base58(sha256(key)[:20])"""
    digest = sha256(signing_public_key).digest()[:20]
    return base58_encode(digest)


def base58_encode(data: bytes) -> str:
    """Encode bytes to Base58 (Bitcoin alphabet)."""
    alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'

    # Convert bytes to integer
    num = int.from_bytes(data, 'big')

    # Encode
    result = ''
    while num > 0:
        num, remainder = divmod(num, 58)
        result = alphabet[remainder] + result

    # Handle leading zeros
    for byte in data:
        if byte == 0:
            result = alphabet[0] + result
        else:
            break

    return result or alphabet[0]


def hkdf(ikm: bytes, info: bytes, length: int, salt: bytes = None) -> bytes:
    """Simple HKDF implementation using HMAC-SHA256."""
    if salt is None:
        salt = b'\x00' * 32

    # Extract
    prk = hmac.new(salt, ikm, sha256).digest()

    # Expand
    t = b''
    okm = b''
    for i in range((length + 31) // 32):
        t = hmac.new(prk, t + info + bytes([i + 1]), sha256).digest()
        okm += t

    return okm[:length]


def kdf_rk(root_key: bytes, dh_output: bytes) -> tuple:
    """Derive new root key and chain key from root key and DH output."""
    info = b"agentmesh_rk"
    prk = hmac.new(root_key, dh_output, sha256).digest()
    t1 = hmac.new(prk, info + b'\x01', sha256).digest()
    t2 = hmac.new(prk, t1 + info + b'\x02', sha256).digest()
    return t1, t2  # new_root_key, new_chain_key


def kdf_ck(chain_key: bytes) -> tuple:
    """Derive message key and new chain key from chain key."""
    msg_key = hmac.new(chain_key, b'\x01', sha256).digest()
    new_chain_key = hmac.new(chain_key, b'\x02', sha256).digest()
    return msg_key, new_chain_key


def x3dh_initiator(alice_identity_private: bytes, alice_ephemeral_private: bytes,
                   bob_identity_public: bytes, bob_signed_prekey: bytes,
                   bob_one_time_prekey: bytes = None) -> bytes:
    """Perform X3DH as initiator and return shared secret."""
    dh1 = crypto_scalarmult(alice_identity_private, bob_signed_prekey)
    dh2 = crypto_scalarmult(alice_ephemeral_private, bob_identity_public)
    dh3 = crypto_scalarmult(alice_ephemeral_private, bob_signed_prekey)

    if bob_one_time_prekey:
        dh4 = crypto_scalarmult(alice_ephemeral_private, bob_one_time_prekey)
        dh_concat = dh1 + dh2 + dh3 + dh4
    else:
        dh_concat = dh1 + dh2 + dh3

    return hkdf(dh_concat, b"X3DH", 32)


def generate_vectors():
    """Generate all test vectors."""
    vectors = {}

    # ==========  1. AMID Vectors ==========
    signing_key = SigningKey.generate()
    exchange_key = PrivateKey.generate()

    signing_public = bytes(signing_key.verify_key)
    exchange_public = bytes(exchange_key.public_key)
    amid = derive_amid(signing_public)

    vectors['amid'] = {
        'signingPrivateKey': b64(bytes(signing_key)),
        'signingPublicKey': b64(signing_public),
        'exchangePrivateKey': b64(bytes(exchange_key)),
        'exchangePublicKey': b64(exchange_public),
        'expectedAmid': amid,
    }

    # ==========  2. X3DH Vectors ==========
    # Alice (initiator)
    alice_signing = SigningKey.generate()
    alice_identity = PrivateKey.generate()
    alice_ephemeral = PrivateKey.generate()

    # Bob (responder)
    bob_signing = SigningKey.generate()
    bob_identity = PrivateKey.generate()
    bob_signed_prekey = PrivateKey.generate()
    bob_one_time_prekey = PrivateKey.generate()

    # Sign Bob's prekey
    bob_signed_prekey_signature = bob_signing.sign(bytes(bob_signed_prekey.public_key)).signature

    # Compute shared secret
    shared_secret = x3dh_initiator(
        bytes(alice_identity),
        bytes(alice_ephemeral),
        bytes(bob_identity.public_key),
        bytes(bob_signed_prekey.public_key),
        bytes(bob_one_time_prekey.public_key),
    )

    vectors['x3dh'] = {
        'aliceIdentityPrivate': b64(bytes(alice_identity)),
        'aliceIdentityPublic': b64(bytes(alice_identity.public_key)),
        'aliceEphemeralPrivate': b64(bytes(alice_ephemeral)),
        'aliceEphemeralPublic': b64(bytes(alice_ephemeral.public_key)),
        'bobIdentityPrivate': b64(bytes(bob_identity)),
        'bobIdentityPublic': b64(bytes(bob_identity.public_key)),
        'bobSignedPrekeyPrivate': b64(bytes(bob_signed_prekey)),
        'bobSignedPrekeyPublic': b64(bytes(bob_signed_prekey.public_key)),
        'bobSignedPrekeySignature': b64(bob_signed_prekey_signature),
        'bobSigningPublicKey': b64(bytes(bob_signing.verify_key)),
        'bobOneTimePrekeyPrivate': b64(bytes(bob_one_time_prekey)),
        'bobOneTimePrekeyPublic': b64(bytes(bob_one_time_prekey.public_key)),
        'expectedSharedSecret': b64(shared_secret),
    }

    # ==========  3. HKDF Vectors ==========
    ikm = random(32)
    salt = random(32)
    info = b"test_info"
    hkdf_output = hkdf(ikm, info, 32, salt)

    vectors['hkdf'] = {
        'ikm': b64(ikm),
        'salt': b64(salt),
        'info': 'test_info',
        'length': 32,
        'expectedOutput': b64(hkdf_output),
    }

    # ==========  4. Chain Key Derivation Vectors ==========
    chain_key = random(32)
    msg_key, next_chain_key = kdf_ck(chain_key)

    vectors['chainKeyDerivation'] = {
        'chainKey': b64(chain_key),
        'expectedMessageKey': b64(msg_key),
        'expectedNextChainKey': b64(next_chain_key),
    }

    # ==========  5. Root Key Derivation Vectors ==========
    root_key = random(32)
    dh_output = random(32)
    new_root, new_chain = kdf_rk(root_key, dh_output)

    vectors['rootKeyDerivation'] = {
        'rootKey': b64(root_key),
        'dhOutput': b64(dh_output),
        'expectedNewRoot': b64(new_root),
        'expectedChainKey': b64(new_chain),
    }

    # ==========  6. XSalsa20-Poly1305 (SecretBox) Vectors ==========
    secret_key = random(32)
    plaintext = b'Hello, World!'
    box = SecretBox(secret_key)
    ciphertext = box.encrypt(plaintext)  # includes 24-byte nonce
    nonce = ciphertext[:24]
    encrypted = ciphertext[24:]

    vectors['secretbox'] = {
        'key': b64(secret_key),
        'nonce': b64(nonce),
        'plaintext': 'Hello, World!',
        'expectedCiphertext': b64(ciphertext),  # nonce + encrypted
    }

    # ==========  7. Base58 Vectors ==========
    test_bytes = bytes([0, 0, 0, 1, 2, 3, 4, 5])
    vectors['base58'] = {
        'input': test_bytes.hex(),
        'expectedBase58': base58_encode(test_bytes),
    }

    # ==========  8. Ed25519 Signature Vectors ==========
    sign_key = SigningKey.generate()
    message = b'Test message to sign'
    signature = sign_key.sign(message).signature

    vectors['signature'] = {
        'privateKey': b64(bytes(sign_key)),
        'publicKey': b64(bytes(sign_key.verify_key)),
        'message': 'Test message to sign',
        'expectedSignature': b64(signature),
    }

    return vectors


def main():
    vectors = generate_vectors()

    # Output as JSON
    print("// Generated test vectors - copy to test-vectors.ts")
    print(json.dumps(vectors, indent=2))

    # Also write to file
    with open('generated-vectors.json', 'w') as f:
        json.dump(vectors, f, indent=2)

    print("\n\nVectors saved to generated-vectors.json")


if __name__ == '__main__':
    main()
