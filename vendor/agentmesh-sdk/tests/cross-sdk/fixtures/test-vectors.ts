/**
 * Cross-SDK Test Vectors
 *
 * These test vectors are used to verify compatibility between
 * TypeScript and Python SDKs.
 *
 * Generated from Python SDK using generate_vectors.py
 * Last updated: 2026-02-01
 */

/**
 * AMID derivation test vectors
 */
export const amidVectors = {
  vector1: {
    signingPrivateKey: 'pjHmygmU0ZQD3QWvkb0yJRvPDh8x2z5OW3h1Ibm0/vM=',
    signingPublicKey: 'wf/dtFozyn5zE2dS06RTvpK8cWHbIpkeXwYVrimqtXY=',
    exchangePrivateKey: 'vhwINF7NLrNIn+taT+NeUYf7xnJL0yYeM8eeBgkB1m0=',
    exchangePublicKey: '2gdaLFZxpg2Rf/57LLkBAYgVwEeyOuurMMiBv5PDenE=',
    expectedAmid: 'jZ6CK1dpcZD4oPYScUJvifP7BtJ',
  },
};

/**
 * X3DH test vectors
 */
export const x3dhVectors = {
  vector1: {
    // Initiator (Alice)
    aliceIdentityPrivate: 'VBgE2WW33wRZAPykVqR2eCV9T77YRCZEM2oOtYWiNiA=',
    aliceIdentityPublic: 'ms7gM9gekVMuMIRZRYCcRPT0I6e624UTI0dJbR8Rlzc=',
    aliceEphemeralPrivate: 'HGJlC5RW+FqcQ5Bvl4fnMs5HXCpNvMM1k6m1eDOWXrk=',
    aliceEphemeralPublic: '4byaanYlHMNL8UNSzeSnhYuyApyIWKhigx33H+y1egk=',

    // Responder (Bob)
    bobIdentityPrivate: 'oe8xerT6oRxqdJuhnHJ954j/IkiPcjy2WTjAOqEzhTE=',
    bobIdentityPublic: '5EKAKDB0IsnH9GKdKWxxjTGewY4jSt3gdACN6HxCSi0=',
    bobSignedPrekeyPrivate: 'cco7Tuq8aXp3OtaiV5dCGmd708p7ap38PfpmGc6wRAc=',
    bobSignedPrekeyPublic: 'WvzlPN0TRO4+d2V6JgoSM+93IqMTjQ5PHMgHyKTlfHc=',
    bobSignedPrekeySignature: 'Dk+YJi1c9eZOAoKo/GNqyZ3CZfvDC2byV34JsZ4EU0IzYdvsZ5vRn5+yKeZjhNUnD0CLbqQ1GrduhPxgjwNbAA==',
    bobSigningPublicKey: 'E7RAq5XydbCiV7fwsMJety83LB8m6376gWTC31Gj1Z8=',
    bobOneTimePrekeyPrivate: 'ymsAYx2mt7O/4+3w7p+lyP2izRqCqm9OAADU/u+bf7c=',
    bobOneTimePrekeyPublic: 'XJAXQ+2Wz8QLBz6oFNcEmYffEu/h/Wi21WWZ/Dp3s14=',

    // Expected outputs
    expectedSharedSecret: '1ZiCFlHzWlk4cuOjf7QW+dRcrrYWzvYwcMCXaqH/BG4=',
  },
};

/**
 * HKDF test vectors
 */
export const hkdfVectors = {
  vector1: {
    ikm: 'sqKudNiJGUVlOiUrVmdhrFr60ofPXIZV4ehd47dppQU=',
    salt: 'xdRHD48ejyTF4ZGbDpZIzmD0BB1cEQhN2Dq5BIEgIn8=',
    info: 'test_info',
    length: 32,
    expectedOutput: 'pwAVr0UbE884Zv7kHZRKw+lrBnNlTichVndzQ4t26dI=',
  },
};

/**
 * Double Ratchet test vectors
 */
export const ratchetVectors = {
  // Chain key derivation (matches Python's kdf_ck)
  chainKeyDerivation: {
    chainKey: '1jvClZHVyaRypcO9oyZs3If3Iat6LzZr13cUR9Q8PXY=',
    expectedMessageKey: 'hvdOdo2FLDzbCHGiMo2VVrs1kXRQ/1L1WRNfhCgbyN0=',
    expectedNextChainKey: 's5FElrzM21xYX2qg+QwC9hPA4DdUNkGJEZ1vLltw7EU=',
  },

  // Root key derivation (matches Python's kdf_rk)
  rootKeyDerivation: {
    rootKey: 'CWb+FDuGVCobFO1W9Xf01t5FEoUKv6T9jbrKKKYD1Po=',
    dhOutput: 'MMBr36VNidbAUuvW6MnSzj/eVR8+1hjIgyx01L4jmq8=',
    expectedNewRoot: 'natrYz9sDOff6EpD6R3Yy01r11xj+1odBpQhs4C/TmM=',
    expectedChainKey: 'Iyb+FYL+k6JGwluIMvZXLDgo6gCl8NRg9vaXqHia3Og=',
  },
};

/**
 * XSalsa20-Poly1305 (NaCl SecretBox) test vectors
 * Note: This replaces AES-GCM for Python SDK compatibility
 */
export const secretboxVectors = {
  vector1: {
    key: 'XTeviRC7wNWPcLiFAslkeSDagfCX7skQOxMz+1hNA5c=',
    nonce: 'lke+rObpmDeAAC62SQR52cuwwS6WaIFn',
    plaintext: 'Hello, World!',
    // Full ciphertext includes 24-byte nonce prefix
    expectedCiphertext: 'lke+rObpmDeAAC62SQR52cuwwS6WaIFnLeeZsnxezMifDIJ9KWIY4Xq/QVtOLdMcWFAgek8=',
  },
};

/**
 * Base58 encoding test vectors
 */
export const base58Vectors = {
  vector1: {
    input: '0000000102030405',
    expectedBase58: '1117bWpTW',
  },
};

/**
 * Ed25519 signature test vectors
 */
export const signatureVectors = {
  vector1: {
    privateKey: 'IxcRHIYNu3GiBxk2kmebl22LTYsVEg9fh7Wk6cj+liI=',
    publicKey: 'StpzcHb4fS73xN5EbfOCMmDM9yxRxIOwV9U9ow9ImPg=',
    message: 'Test message to sign',
    expectedSignature: 'vrni9EEOQ2MIzsqEumEDXEMSr6ua55BhiyhVo5okBGMr/52htps4HpC/8B2vgNxGlEdfH2+gktAfVyqvSJyxDw==',
  },
};

/**
 * Helper to convert base64 to Uint8Array
 */
export function base64ToBytes(base64: string): Uint8Array {
  if (!base64) return new Uint8Array(0);
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Helper to convert Uint8Array to base64
 */
export function bytesToBase64(bytes: Uint8Array): string {
  let binary = '';
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]!);
  }
  return btoa(binary);
}

/**
 * Helper to convert hex to Uint8Array
 */
export function hexToBytes(hex: string): Uint8Array {
  if (!hex) return new Uint8Array(0);
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < hex.length; i += 2) {
    bytes[i / 2] = parseInt(hex.substring(i, i + 2), 16);
  }
  return bytes;
}
