/**
 * HKDF (HMAC-based Key Derivation Function) implementation.
 * Uses Web Crypto API for HMAC-SHA256.
 */

/**
 * Helper to convert Uint8Array to ArrayBuffer for Web Crypto API compatibility.
 * TypeScript's strict BufferSource typing requires fresh ArrayBuffer instances.
 */
function toArrayBuffer(data: Uint8Array): ArrayBuffer {
  const buffer = new ArrayBuffer(data.length);
  new Uint8Array(buffer).set(data);
  return buffer;
}

/**
 * HKDF Extract step - extracts a pseudorandom key from input key material.
 */
async function hkdfExtract(salt: Uint8Array, ikm: Uint8Array): Promise<Uint8Array> {
  const saltData = salt.length > 0 ? salt : new Uint8Array(32);
  // Import salt as HMAC key
  const saltKey = await crypto.subtle.importKey(
    'raw',
    toArrayBuffer(saltData),
    { name: 'HMAC', hash: 'SHA-256' },
    false,
    ['sign']
  );

  // HMAC(salt, ikm)
  const prk = await crypto.subtle.sign('HMAC', saltKey, toArrayBuffer(ikm));
  return new Uint8Array(prk);
}

/**
 * HKDF Expand step - expands pseudorandom key to desired length.
 */
async function hkdfExpand(prk: Uint8Array, info: Uint8Array, length: number): Promise<Uint8Array> {
  // Import PRK as HMAC key
  const prkKey = await crypto.subtle.importKey(
    'raw',
    toArrayBuffer(prk),
    { name: 'HMAC', hash: 'SHA-256' },
    false,
    ['sign']
  );

  const hashLen = 32; // SHA-256 output length
  const n = Math.ceil(length / hashLen);
  const okm = new Uint8Array(n * hashLen);

  let t = new Uint8Array(0);
  for (let i = 0; i < n; i++) {
    // T(i) = HMAC(PRK, T(i-1) || info || i+1)
    const input = new Uint8Array(t.length + info.length + 1);
    input.set(t, 0);
    input.set(info, t.length);
    input[t.length + info.length] = i + 1;

    const block = await crypto.subtle.sign('HMAC', prkKey, toArrayBuffer(input));
    t = new Uint8Array(block);
    okm.set(t, i * hashLen);
  }

  return okm.slice(0, length);
}

/**
 * HKDF - Derive a key from input key material.
 *
 * @param ikm - Input key material
 * @param salt - Salt (optional, defaults to zeros)
 * @param info - Context and application-specific information
 * @param length - Length of output key in bytes
 * @returns Derived key
 */
export async function hkdf(
  ikm: Uint8Array,
  salt: Uint8Array,
  info: Uint8Array,
  length: number
): Promise<Uint8Array> {
  const prk = await hkdfExtract(salt, ikm);
  return hkdfExpand(prk, info, length);
}

/**
 * Simple HKDF with default salt.
 */
export async function hkdfSimple(
  ikm: Uint8Array,
  info: string | Uint8Array,
  length: number
): Promise<Uint8Array> {
  const infoBytes = typeof info === 'string' ? new TextEncoder().encode(info) : info;
  const salt = new Uint8Array(32); // Zero salt
  return hkdf(ikm, salt, infoBytes, length);
}

/**
 * Derive two keys from input - used for Double Ratchet.
 * Returns [root_key, chain_key] pair.
 */
export async function kdfRK(
  rootKey: Uint8Array,
  dhOutput: Uint8Array
): Promise<[Uint8Array, Uint8Array]> {
  const info = new TextEncoder().encode('agentmesh_rk');
  const output = await hkdf(dhOutput, rootKey, info, 64);
  return [output.slice(0, 32), output.slice(32, 64)];
}

/**
 * Derive message key and new chain key from chain key.
 * Returns [message_key, new_chain_key] pair.
 */
export async function kdfCK(chainKey: Uint8Array): Promise<[Uint8Array, Uint8Array]> {
  // Import chain key as HMAC key
  const key = await crypto.subtle.importKey(
    'raw',
    toArrayBuffer(chainKey),
    { name: 'HMAC', hash: 'SHA-256' },
    false,
    ['sign']
  );

  // Message key = HMAC(chain_key, 0x01)
  const msgKeyData = await crypto.subtle.sign('HMAC', key, toArrayBuffer(new Uint8Array([0x01])));

  // New chain key = HMAC(chain_key, 0x02)
  const newChainData = await crypto.subtle.sign('HMAC', key, toArrayBuffer(new Uint8Array([0x02])));

  return [new Uint8Array(msgKeyData), new Uint8Array(newChainData)];
}
