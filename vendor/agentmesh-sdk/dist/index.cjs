'use strict';

var chunkBPYP43TA_cjs = require('./chunk-BPYP43TA.cjs');
var chunkUBUGIENK_cjs = require('./chunk-UBUGIENK.cjs');
var chunkFAEZQCEA_cjs = require('./chunk-FAEZQCEA.cjs');
var chunkC7KJHFTP_cjs = require('./chunk-C7KJHFTP.cjs');
require('./chunk-FK3FEKXY.cjs');
var chunkFNHOFD2H_cjs = require('./chunk-FNHOFD2H.cjs');
var sodium = require('libsodium-wrappers');
var crypto$1 = require('crypto');
var http = require('http');

function _interopDefault (e) { return e && e.__esModule ? e : { default: e }; }

var sodium__default = /*#__PURE__*/_interopDefault(sodium);

// src/encryption/hkdf.ts
function toArrayBuffer(data) {
  const buffer = new ArrayBuffer(data.length);
  new Uint8Array(buffer).set(data);
  return buffer;
}
async function hkdfExtract(salt, ikm) {
  const saltData = salt.length > 0 ? salt : new Uint8Array(32);
  const saltKey = await crypto.subtle.importKey(
    "raw",
    toArrayBuffer(saltData),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"]
  );
  const prk = await crypto.subtle.sign("HMAC", saltKey, toArrayBuffer(ikm));
  return new Uint8Array(prk);
}
async function hkdfExpand(prk, info, length) {
  const prkKey = await crypto.subtle.importKey(
    "raw",
    toArrayBuffer(prk),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"]
  );
  const hashLen = 32;
  const n = Math.ceil(length / hashLen);
  const okm = new Uint8Array(n * hashLen);
  let t = new Uint8Array(0);
  for (let i = 0; i < n; i++) {
    const input = new Uint8Array(t.length + info.length + 1);
    input.set(t, 0);
    input.set(info, t.length);
    input[t.length + info.length] = i + 1;
    const block = await crypto.subtle.sign("HMAC", prkKey, toArrayBuffer(input));
    t = new Uint8Array(block);
    okm.set(t, i * hashLen);
  }
  return okm.slice(0, length);
}
async function hkdf(ikm, salt, info, length) {
  const prk = await hkdfExtract(salt, ikm);
  return hkdfExpand(prk, info, length);
}
async function hkdfSimple(ikm, info, length) {
  const infoBytes = typeof info === "string" ? new TextEncoder().encode(info) : info;
  const salt = new Uint8Array(32);
  return hkdf(ikm, salt, infoBytes, length);
}
async function kdfRK(rootKey, dhOutput) {
  const info = new TextEncoder().encode("agentmesh_rk");
  const output = await hkdf(dhOutput, rootKey, info, 64);
  return [output.slice(0, 32), output.slice(32, 64)];
}
async function kdfCK(chainKey) {
  const key = await crypto.subtle.importKey(
    "raw",
    toArrayBuffer(chainKey),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"]
  );
  const msgKeyData = await crypto.subtle.sign("HMAC", key, toArrayBuffer(new Uint8Array([1])));
  const newChainData = await crypto.subtle.sign("HMAC", key, toArrayBuffer(new Uint8Array([2])));
  return [new Uint8Array(msgKeyData), new Uint8Array(newChainData)];
}

// src/encryption/prekey.ts
var X25519_PKCS8_PREFIX = new Uint8Array([
  48,
  46,
  2,
  1,
  0,
  48,
  5,
  6,
  3,
  43,
  101,
  110,
  4,
  34,
  4,
  32
]);
var X25519_SPKI_PREFIX = new Uint8Array([
  48,
  42,
  48,
  5,
  6,
  3,
  43,
  101,
  110,
  3,
  33,
  0
]);
var PREKEY_CONFIG = {
  ONE_TIME_PREKEY_COUNT: 100,
  PREKEY_LOW_THRESHOLD: 20,
  SIGNED_PREKEY_ROTATION_DAYS: 7,
  SIGNED_PREKEY_GRACE_PERIOD_HOURS: 24
};
async function generateX25519Keypair() {
  const keyPair = await crypto.subtle.generateKey("X25519", true, [
    "deriveBits"
  ]);
  const publicKeySpki = await crypto.subtle.exportKey("spki", keyPair.publicKey);
  const privateKeyPkcs8 = await crypto.subtle.exportKey("pkcs8", keyPair.privateKey);
  const publicKey = new Uint8Array(publicKeySpki).slice(X25519_SPKI_PREFIX.length);
  const privateKey = new Uint8Array(privateKeyPkcs8).slice(X25519_PKCS8_PREFIX.length);
  return { publicKey, privateKey };
}
async function importX25519PrivateKey(privateKey) {
  const pkcs8 = new Uint8Array(X25519_PKCS8_PREFIX.length + privateKey.length);
  pkcs8.set(X25519_PKCS8_PREFIX, 0);
  pkcs8.set(privateKey, X25519_PKCS8_PREFIX.length);
  return crypto.subtle.importKey(
    "pkcs8",
    pkcs8,
    { name: "X25519" },
    false,
    ["deriveBits"]
  );
}
async function importX25519PublicKey(publicKey) {
  const spki = new Uint8Array(X25519_SPKI_PREFIX.length + publicKey.length);
  spki.set(X25519_SPKI_PREFIX, 0);
  spki.set(publicKey, X25519_SPKI_PREFIX.length);
  return crypto.subtle.importKey(
    "spki",
    spki,
    { name: "X25519" },
    false,
    []
  );
}
async function x25519DH(privateKey, publicKey) {
  const privKey = await importX25519PrivateKey(privateKey);
  const pubKey = await importX25519PublicKey(publicKey);
  const sharedBits = await crypto.subtle.deriveBits(
    { name: "X25519", public: pubKey },
    privKey,
    256
  );
  return new Uint8Array(sharedBits);
}
async function generateSignedPrekey(identity, id) {
  const { publicKey, privateKey } = await generateX25519Keypair();
  const signature = await identity.sign(publicKey);
  return {
    id,
    publicKey,
    privateKey,
    signature,
    createdAt: /* @__PURE__ */ new Date()
  };
}
async function generateOneTimePrekeys(startId, count) {
  const prekeys = [];
  for (let i = 0; i < count; i++) {
    const { publicKey, privateKey } = await generateX25519Keypair();
    prekeys.push({
      id: startId + i,
      publicKey,
      privateKey
    });
  }
  return prekeys;
}
function serializePrekeyBundle(bundle) {
  const toBase642 = (bytes) => {
    const binary = String.fromCharCode(...bytes);
    return btoa(binary);
  };
  return {
    identity_key: toBase642(bundle.identityKey),
    signed_prekey: toBase642(bundle.signedPrekey),
    signed_prekey_signature: toBase642(bundle.signedPrekeySignature),
    signed_prekey_id: bundle.signedPrekeyId,
    one_time_prekeys: bundle.oneTimePrekeys.map((pk) => ({
      id: pk.id,
      key: toBase642(pk.key)
    })),
    uploaded_at: bundle.uploadedAt?.toISOString()
  };
}
function deserializePrekeyBundle(data) {
  const stripKeyPrefix = (s) => {
    if (s.startsWith("ed25519:")) return s.slice(8);
    if (s.startsWith("x25519:")) return s.slice(7);
    return s;
  };
  const fromBase642 = (b64) => {
    const binary = atob(stripKeyPrefix(b64));
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  };
  return {
    identityKey: fromBase642(data.identity_key),
    signedPrekey: fromBase642(data.signed_prekey),
    signedPrekeySignature: fromBase642(data.signed_prekey_signature),
    signedPrekeyId: data.signed_prekey_id,
    oneTimePrekeys: (data.one_time_prekeys ?? []).map((pk) => ({
      id: pk.id,
      key: fromBase642(pk.key)
    })),
    uploadedAt: data.uploaded_at ? new Date(data.uploaded_at) : void 0
  };
}
var PrekeyManager = class {
  identity;
  storage;
  state = null;
  storagePath = "prekeys/state.json";
  constructor(identity, storage) {
    this.identity = identity;
    this.storage = storage;
  }
  /**
   * Load or initialize prekeys.
   */
  async loadOrInitialize() {
    const data = await this.storage.get(this.storagePath);
    if (data) {
      try {
        this.state = this.deserializeState(data);
        const age = Date.now() - this.state.signedPrekeyCreated.getTime();
        const maxAge = PREKEY_CONFIG.SIGNED_PREKEY_ROTATION_DAYS * 24 * 60 * 60 * 1e3;
        if (age > maxAge) {
          await this.rotateSignedPrekey();
        }
        if (this.needsReplenishment()) {
          await this.replenishPrekeys();
        }
        return await this.buildBundle();
      } catch {
      }
    }
    return this.generateInitialPrekeys();
  }
  /**
   * Generate initial prekey bundle.
   */
  async generateInitialPrekeys() {
    const signedPrekey = await generateSignedPrekey(this.identity, 1);
    const oneTimePrekeys = await generateOneTimePrekeys(1, PREKEY_CONFIG.ONE_TIME_PREKEY_COUNT);
    const prekeyMap = /* @__PURE__ */ new Map();
    const prekeyPubMap = /* @__PURE__ */ new Map();
    for (const pk of oneTimePrekeys) {
      prekeyMap.set(pk.id, pk.privateKey);
      prekeyPubMap.set(pk.id, pk.publicKey);
    }
    this.state = {
      signedPrekeyId: 1,
      signedPrekeyPrivate: signedPrekey.privateKey,
      signedPrekeyPublic: signedPrekey.publicKey,
      signedPrekeyCreated: /* @__PURE__ */ new Date(),
      oneTimePrekeys: prekeyMap,
      oneTimePrekeyPublicKeys: prekeyPubMap,
      nextPrekeyId: PREKEY_CONFIG.ONE_TIME_PREKEY_COUNT + 1,
      consumedPrekeyIds: []
    };
    await this.saveState();
    return {
      identityKey: this.identity.getExchangePublicKeyRaw(),
      signedPrekey: signedPrekey.publicKey,
      signedPrekeySignature: signedPrekey.signature,
      signedPrekeyId: 1,
      oneTimePrekeys: oneTimePrekeys.map((pk) => ({ id: pk.id, key: pk.publicKey })),
      uploadedAt: /* @__PURE__ */ new Date()
    };
  }
  /**
   * Rotate signed prekey (every 7 days).
   */
  async rotateSignedPrekey() {
    if (!this.state) {
      throw new Error("Prekey state not initialized");
    }
    this.state.oldSignedPrekeyPrivate = this.state.signedPrekeyPrivate;
    this.state.oldSignedPrekeyId = this.state.signedPrekeyId;
    this.state.oldSignedPrekeyExpires = new Date(
      Date.now() + PREKEY_CONFIG.SIGNED_PREKEY_GRACE_PERIOD_HOURS * 60 * 60 * 1e3
    );
    const newId = this.state.signedPrekeyId + 1;
    const signedPrekey = await generateSignedPrekey(this.identity, newId);
    this.state.signedPrekeyId = newId;
    this.state.signedPrekeyPrivate = signedPrekey.privateKey;
    this.state.signedPrekeyPublic = signedPrekey.publicKey;
    this.state.signedPrekeyCreated = /* @__PURE__ */ new Date();
    await this.saveState();
    return await this.buildBundle();
  }
  /**
   * Replenish one-time prekeys when running low.
   */
  async replenishPrekeys() {
    if (!this.state) {
      throw new Error("Prekey state not initialized");
    }
    const currentCount = this.state.oneTimePrekeys.size;
    const countToGenerate = PREKEY_CONFIG.ONE_TIME_PREKEY_COUNT - currentCount;
    if (countToGenerate <= 0) {
      return [];
    }
    const newPrekeys = await generateOneTimePrekeys(this.state.nextPrekeyId, countToGenerate);
    for (const pk of newPrekeys) {
      this.state.oneTimePrekeys.set(pk.id, pk.privateKey);
      this.state.oneTimePrekeyPublicKeys.set(pk.id, pk.publicKey);
    }
    this.state.nextPrekeyId += countToGenerate;
    await this.saveState();
    return newPrekeys.map((pk) => ({ id: pk.id, key: pk.publicKey }));
  }
  /**
   * Get signed prekey private key by ID.
   */
  getSignedPrekeyPrivate(id) {
    if (!this.state) return null;
    if (id === this.state.signedPrekeyId) {
      return this.state.signedPrekeyPrivate;
    }
    if (this.state.oldSignedPrekeyId === id && this.state.oldSignedPrekeyExpires && /* @__PURE__ */ new Date() < this.state.oldSignedPrekeyExpires) {
      return this.state.oldSignedPrekeyPrivate ?? null;
    }
    return null;
  }
  /**
   * Get signed prekey public key by ID.
   */
  getSignedPrekeyPublic(id) {
    if (!this.state) return null;
    if (id === this.state.signedPrekeyId) {
      return this.state.signedPrekeyPublic;
    }
    return null;
  }
  /**
   * Get one-time prekey private key by ID.
   */
  getOneTimePrekeyPrivate(id) {
    if (!this.state) return null;
    return this.state.oneTimePrekeys.get(id) ?? null;
  }
  /**
   * Consume a one-time prekey (mark as used).
   */
  async consumePrekey(id) {
    if (!this.state) return;
    this.state.oneTimePrekeys.delete(id);
    this.state.oneTimePrekeyPublicKeys.delete(id);
    this.state.consumedPrekeyIds.push(id);
    if (this.state.consumedPrekeyIds.length > 1e3) {
      this.state.consumedPrekeyIds = this.state.consumedPrekeyIds.slice(-1e3);
    }
    await this.saveState();
  }
  /**
   * Check if prekey was already consumed.
   */
  isPrekeyConsumed(id) {
    return this.state?.consumedPrekeyIds.includes(id) ?? false;
  }
  /**
   * Check if replenishment is needed.
   */
  needsReplenishment() {
    if (!this.state) return true;
    return this.state.oneTimePrekeys.size < PREKEY_CONFIG.PREKEY_LOW_THRESHOLD;
  }
  /**
   * Get remaining prekey count.
   */
  remainingPrekeyCount() {
    return this.state?.oneTimePrekeys.size ?? 0;
  }
  /**
   * Build prekey bundle from current state.
   * Re-signs the signed prekey and includes stored public keys.
   */
  async buildBundle() {
    if (!this.state) {
      throw new Error("Prekey state not initialized");
    }
    const signedPrekeySignature = await this.identity.sign(this.state.signedPrekeyPublic);
    const oneTimePrekeys = [];
    for (const [id, publicKey] of this.state.oneTimePrekeyPublicKeys) {
      oneTimePrekeys.push({ id, key: publicKey });
    }
    return {
      identityKey: this.identity.getExchangePublicKeyRaw(),
      signedPrekey: this.state.signedPrekeyPublic,
      signedPrekeySignature,
      signedPrekeyId: this.state.signedPrekeyId,
      oneTimePrekeys,
      uploadedAt: /* @__PURE__ */ new Date()
    };
  }
  /**
   * Serialize state for storage.
   */
  serializeState() {
    if (!this.state) {
      throw new Error("No state to serialize");
    }
    const toBase642 = (bytes) => {
      const binary = String.fromCharCode(...bytes);
      return btoa(binary);
    };
    const oneTimePrekeys = {};
    for (const [id, privateKey] of this.state.oneTimePrekeys) {
      oneTimePrekeys[String(id)] = toBase642(privateKey);
    }
    const oneTimePrekeyPublicKeys = {};
    for (const [id, publicKey] of this.state.oneTimePrekeyPublicKeys) {
      oneTimePrekeyPublicKeys[String(id)] = toBase642(publicKey);
    }
    const data = {
      signedPrekeyId: this.state.signedPrekeyId,
      signedPrekeyPrivate: toBase642(this.state.signedPrekeyPrivate),
      signedPrekeyPublic: toBase642(this.state.signedPrekeyPublic),
      signedPrekeyCreated: this.state.signedPrekeyCreated.toISOString(),
      oneTimePrekeys,
      oneTimePrekeyPublicKeys,
      nextPrekeyId: this.state.nextPrekeyId,
      consumedPrekeyIds: this.state.consumedPrekeyIds,
      oldSignedPrekeyPrivate: this.state.oldSignedPrekeyPrivate ? toBase642(this.state.oldSignedPrekeyPrivate) : void 0,
      oldSignedPrekeyId: this.state.oldSignedPrekeyId,
      oldSignedPrekeyExpires: this.state.oldSignedPrekeyExpires?.toISOString()
    };
    return new TextEncoder().encode(JSON.stringify(data, null, 2));
  }
  /**
   * Deserialize state from storage.
   */
  deserializeState(data) {
    const fromBase642 = (b64) => {
      const binary = atob(b64);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) {
        bytes[i] = binary.charCodeAt(i);
      }
      return bytes;
    };
    const json = JSON.parse(new TextDecoder().decode(data));
    const oneTimePrekeys = /* @__PURE__ */ new Map();
    for (const [id, b64] of Object.entries(json.oneTimePrekeys)) {
      oneTimePrekeys.set(parseInt(id, 10), fromBase642(b64));
    }
    const oneTimePrekeyPublicKeys = /* @__PURE__ */ new Map();
    if (json.oneTimePrekeyPublicKeys) {
      for (const [id, b64] of Object.entries(json.oneTimePrekeyPublicKeys)) {
        oneTimePrekeyPublicKeys.set(parseInt(id, 10), fromBase642(b64));
      }
    }
    return {
      signedPrekeyId: json.signedPrekeyId,
      signedPrekeyPrivate: fromBase642(json.signedPrekeyPrivate),
      signedPrekeyPublic: fromBase642(json.signedPrekeyPublic),
      signedPrekeyCreated: new Date(json.signedPrekeyCreated),
      oneTimePrekeys,
      oneTimePrekeyPublicKeys,
      nextPrekeyId: json.nextPrekeyId,
      consumedPrekeyIds: json.consumedPrekeyIds ?? [],
      oldSignedPrekeyPrivate: json.oldSignedPrekeyPrivate ? fromBase642(json.oldSignedPrekeyPrivate) : void 0,
      oldSignedPrekeyId: json.oldSignedPrekeyId,
      oldSignedPrekeyExpires: json.oldSignedPrekeyExpires ? new Date(json.oldSignedPrekeyExpires) : void 0
    };
  }
  /**
   * Save state to storage.
   */
  async saveState() {
    await this.storage.set(this.storagePath, this.serializeState());
  }
};

// src/encryption/x3dh.ts
var X3DHKeyExchange = class {
  /**
   * Perform X3DH as the initiator (Alice).
   *
   * @param ourIdentity - Our identity (provides exchange private key)
   * @param theirBundle - Their published prekey bundle
   * @param theirSigningPublicKey - Their signing public key (to verify prekey signature)
   * @returns The shared secret and initiator message
   */
  static async initiator(ourIdentity, theirBundle, theirSigningPublicKey) {
    const signatureValid = await chunkBPYP43TA_cjs.Identity.verifySignatureRaw(
      theirSigningPublicKey,
      theirBundle.signedPrekey,
      theirBundle.signedPrekeySignature
    );
    if (!signatureValid) {
      throw new Error("Invalid signed prekey signature");
    }
    const ephemeral = await generateX25519Keypair();
    const ourIdentityPrivate = ourIdentity.getExchangePrivateKeyRaw();
    const dh1 = await x25519DH(ourIdentityPrivate, theirBundle.signedPrekey);
    const dh2 = await x25519DH(ephemeral.privateKey, theirBundle.identityKey);
    const dh3 = await x25519DH(ephemeral.privateKey, theirBundle.signedPrekey);
    let dhConcat;
    let oneTimePrekeyId;
    const oneTimePrekey = theirBundle.oneTimePrekeys[0];
    if (oneTimePrekey) {
      oneTimePrekeyId = oneTimePrekey.id;
      const dh4 = await x25519DH(ephemeral.privateKey, oneTimePrekey.key);
      dhConcat = new Uint8Array(dh1.length + dh2.length + dh3.length + dh4.length);
      dhConcat.set(dh1, 0);
      dhConcat.set(dh2, dh1.length);
      dhConcat.set(dh3, dh1.length + dh2.length);
      dhConcat.set(dh4, dh1.length + dh2.length + dh3.length);
    } else {
      dhConcat = new Uint8Array(dh1.length + dh2.length + dh3.length);
      dhConcat.set(dh1, 0);
      dhConcat.set(dh2, dh1.length);
      dhConcat.set(dh3, dh1.length + dh2.length);
    }
    const sharedSecret = await hkdfSimple(dhConcat, "X3DH", 32);
    const initiatorMessage = {
      identityKey: ourIdentity.getExchangePublicKeyRaw(),
      ephemeralKey: ephemeral.publicKey,
      signedPrekeyId: theirBundle.signedPrekeyId,
      oneTimePrekeyId
    };
    return {
      sharedSecret,
      initiatorMessage,
      ephemeralPrivate: ephemeral.privateKey
    };
  }
  /**
   * Perform X3DH as the responder (Bob).
   *
   * @param ourIdentity - Our identity (provides exchange private key)
   * @param ourSignedPrekeyPrivate - Our signed prekey private key
   * @param ourOneTimePrekeyPrivate - Our one-time prekey private key (if used)
   * @param initiatorMessage - The initiator's X3DH message
   * @returns The shared secret
   */
  static async responder(ourIdentity, ourSignedPrekeyPrivate, ourOneTimePrekeyPrivate, initiatorMessage) {
    const ourIdentityPrivate = ourIdentity.getExchangePrivateKeyRaw();
    const dh1 = await x25519DH(ourSignedPrekeyPrivate, initiatorMessage.identityKey);
    const dh2 = await x25519DH(ourIdentityPrivate, initiatorMessage.ephemeralKey);
    const dh3 = await x25519DH(ourSignedPrekeyPrivate, initiatorMessage.ephemeralKey);
    let dhConcat;
    if (ourOneTimePrekeyPrivate) {
      const dh4 = await x25519DH(ourOneTimePrekeyPrivate, initiatorMessage.ephemeralKey);
      dhConcat = new Uint8Array(dh1.length + dh2.length + dh3.length + dh4.length);
      dhConcat.set(dh1, 0);
      dhConcat.set(dh2, dh1.length);
      dhConcat.set(dh3, dh1.length + dh2.length);
      dhConcat.set(dh4, dh1.length + dh2.length + dh3.length);
    } else {
      dhConcat = new Uint8Array(dh1.length + dh2.length + dh3.length);
      dhConcat.set(dh1, 0);
      dhConcat.set(dh2, dh1.length);
      dhConcat.set(dh3, dh1.length + dh2.length);
    }
    const sharedSecret = await hkdfSimple(dhConcat, "X3DH", 32);
    return {
      sharedSecret,
      initiatorIdentityKey: initiatorMessage.identityKey
    };
  }
  /**
   * Simple X25519 key exchange (fallback when no prekeys available).
   */
  static async simpleKeyExchange(ourPrivateKey, theirPublicKey) {
    return x25519DH(ourPrivateKey, theirPublicKey);
  }
  /**
   * Generate ephemeral keypair.
   */
  static async generateEphemeralKeypair() {
    return generateX25519Keypair();
  }
};
function serializeX3DHMessage(msg) {
  const toBase642 = (bytes) => {
    const binary = String.fromCharCode(...bytes);
    return btoa(binary);
  };
  return {
    identity_key: toBase642(msg.identityKey),
    ephemeral_key: toBase642(msg.ephemeralKey),
    signed_prekey_id: msg.signedPrekeyId,
    one_time_prekey_id: msg.oneTimePrekeyId
  };
}
function deserializeX3DHMessage(data) {
  const fromBase642 = (b64) => {
    const binary = atob(b64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  };
  return {
    identityKey: fromBase642(data.identity_key),
    ephemeralKey: fromBase642(data.ephemeral_key),
    signedPrekeyId: data.signed_prekey_id,
    oneTimePrekeyId: data.one_time_prekey_id
  };
}
var sodiumReady = null;
async function ensureSodiumReady() {
  if (!sodiumReady) {
    sodiumReady = sodium__default.default.ready;
  }
  await sodiumReady;
}
var MAX_SKIP = 1e3;
var DoubleRatchetSession = class _DoubleRatchetSession {
  state;
  isInitiator;
  constructor(state, isInitiator) {
    this.state = state;
    this.isInitiator = isInitiator;
  }
  /**
   * Initialize a new Double Ratchet session from X3DH shared secret.
   *
   * @param sharedSecret - The shared secret from X3DH key exchange
   * @param isInitiator - True if we initiated the session (Alice)
   * @param peerDhPublic - Peer's initial ratchet public key (for responder)
   */
  static async initialize(sharedSecret, isInitiator, peerDhPublic) {
    const { publicKey: dhPublic, privateKey: dhPrivate } = await generateX25519Keypair();
    const state = {
      dhPrivate,
      dhPublic,
      peerDhPublic: peerDhPublic ?? null,
      rootKey: sharedSecret,
      sendChainKey: null,
      recvChainKey: null,
      sendMessageNumber: 0,
      recvMessageNumber: 0,
      previousChainLength: 0,
      skippedKeys: /* @__PURE__ */ new Map()
    };
    const session = new _DoubleRatchetSession(state, isInitiator);
    return session;
  }
  /**
   * Initialize a responder session using the signed prekey as the ratchet keypair.
   *
   * Per Signal Protocol, the responder's initial ratchet key IS the signed prekey,
   * because the initiator encrypted using DH(initiator_ratchet, signedPrekey_pub).
   * The responder decrypts using DH(signedPrekey_priv, initiator_ratchet_pub).
   */
  static async initializeResponder(sharedSecret, signedPrekeyPrivate, signedPrekeyPublic) {
    const state = {
      dhPrivate: signedPrekeyPrivate,
      dhPublic: signedPrekeyPublic,
      peerDhPublic: null,
      // Will be set from the first message's header
      rootKey: sharedSecret,
      sendChainKey: null,
      recvChainKey: null,
      sendMessageNumber: 0,
      recvMessageNumber: 0,
      previousChainLength: 0,
      skippedKeys: /* @__PURE__ */ new Map()
    };
    return new _DoubleRatchetSession(state, false);
  }
  /**
   * Restore session from serialized state.
   */
  static fromState(state, isInitiator) {
    return new _DoubleRatchetSession(state, isInitiator);
  }
  /**
   * Get the current ratchet public key (to include in first message).
   */
  getRatchetPublicKey() {
    return new Uint8Array(this.state.dhPublic);
  }
  /**
   * Initialize the session when we receive peer's key.
   * Just sets the peer's public key without doing a ratchet step.
   * The actual ratchet step happens on first encrypt/decrypt.
   */
  async initializeReceiving(peerDhPublic) {
    this.state.peerDhPublic = peerDhPublic;
  }
  /**
   * Encrypt a plaintext message.
   */
  async encrypt(plaintext) {
    if (!this.state.sendChainKey) {
      if (!this.state.peerDhPublic) {
        throw new Error("Cannot encrypt: peer DH public key not set");
      }
      const dhOutput = await x25519DH(this.state.dhPrivate, this.state.peerDhPublic);
      const [newRoot, sendChain] = await kdfRK(this.state.rootKey, dhOutput);
      this.state.rootKey = newRoot;
      this.state.sendChainKey = sendChain;
    }
    const [messageKey, newChainKey] = await kdfCK(this.state.sendChainKey);
    this.state.sendChainKey = newChainKey;
    const header = {
      dhPublicKey: new Uint8Array(this.state.dhPublic),
      previousChainLength: this.state.previousChainLength,
      messageNumber: this.state.sendMessageNumber
    };
    const ciphertext = await this.aesEncrypt(messageKey, plaintext, this.serializeHeader(header));
    this.state.sendMessageNumber++;
    return { header, ciphertext };
  }
  /**
   * Decrypt an encrypted message.
   */
  async decrypt(message) {
    const { header, ciphertext } = message;
    const skippedKey = this.getSkippedKey(header.dhPublicKey, header.messageNumber);
    if (skippedKey) {
      return this.aesDecrypt(skippedKey, ciphertext, this.serializeHeader(header));
    }
    const isDifferentKey = !this.state.peerDhPublic || !this.bytesEqual(header.dhPublicKey, this.state.peerDhPublic);
    if (isDifferentKey) {
      if (this.state.recvChainKey) {
        await this.skipMessageKeys(this.state.recvMessageNumber + MAX_SKIP);
      }
      this.state.peerDhPublic = header.dhPublicKey;
      await this.dhRatchetStep();
    } else if (!this.state.recvChainKey && this.state.peerDhPublic) {
      const dhOutput = await x25519DH(this.state.dhPrivate, this.state.peerDhPublic);
      const [newRoot, recvChain] = await kdfRK(this.state.rootKey, dhOutput);
      this.state.rootKey = newRoot;
      this.state.recvChainKey = recvChain;
      this.state.recvMessageNumber = 0;
    }
    if (header.messageNumber > this.state.recvMessageNumber) {
      await this.skipMessageKeys(header.messageNumber);
    }
    if (!this.state.recvChainKey) {
      throw new Error("No receiving chain key available");
    }
    const [messageKey, newChainKey] = await kdfCK(this.state.recvChainKey);
    this.state.recvChainKey = newChainKey;
    this.state.recvMessageNumber = header.messageNumber + 1;
    return this.aesDecrypt(messageKey, ciphertext, this.serializeHeader(header));
  }
  /**
   * Perform a DH ratchet step.
   */
  async dhRatchetStep() {
    if (!this.state.peerDhPublic) {
      throw new Error("Cannot ratchet: no peer DH public key");
    }
    const dhOutput1 = await x25519DH(this.state.dhPrivate, this.state.peerDhPublic);
    const [newRoot1, recvChain] = await kdfRK(this.state.rootKey, dhOutput1);
    this.state.recvChainKey = recvChain;
    const { publicKey: newDhPublic, privateKey: newDhPrivate } = await generateX25519Keypair();
    this.state.previousChainLength = this.state.sendMessageNumber;
    this.state.dhPrivate = newDhPrivate;
    this.state.dhPublic = newDhPublic;
    const dhOutput2 = await x25519DH(this.state.dhPrivate, this.state.peerDhPublic);
    const [newRoot2, sendChain] = await kdfRK(newRoot1, dhOutput2);
    this.state.rootKey = newRoot2;
    this.state.sendChainKey = sendChain;
    this.state.sendMessageNumber = 0;
    this.state.recvMessageNumber = 0;
  }
  /**
   * Skip message keys for out-of-order handling.
   */
  async skipMessageKeys(until) {
    if (!this.state.recvChainKey) {
      throw new Error("Cannot skip: no receiving chain key");
    }
    const toSkip = until - this.state.recvMessageNumber;
    if (toSkip > MAX_SKIP) {
      throw new Error(`Too many skipped messages: ${toSkip} > ${MAX_SKIP}`);
    }
    while (this.state.recvMessageNumber < until) {
      const [messageKey, newChainKey] = await kdfCK(this.state.recvChainKey);
      this.storeSkippedKey(this.state.peerDhPublic, this.state.recvMessageNumber, messageKey);
      this.state.recvChainKey = newChainKey;
      this.state.recvMessageNumber++;
    }
  }
  /**
   * Store a skipped message key.
   */
  storeSkippedKey(dhPublic, messageNumber, key) {
    const keyId = this.makeSkippedKeyId(dhPublic, messageNumber);
    this.state.skippedKeys.set(keyId, key);
    if (this.state.skippedKeys.size > MAX_SKIP * 2) {
      const keys = Array.from(this.state.skippedKeys.keys());
      for (let i = 0; i < keys.length - MAX_SKIP; i++) {
        this.state.skippedKeys.delete(keys[i]);
      }
    }
  }
  /**
   * Get a previously skipped message key.
   */
  getSkippedKey(dhPublic, messageNumber) {
    const keyId = this.makeSkippedKeyId(dhPublic, messageNumber);
    const key = this.state.skippedKeys.get(keyId);
    if (key) {
      this.state.skippedKeys.delete(keyId);
      return key;
    }
    return null;
  }
  /**
   * Make a key ID for skipped keys storage.
   */
  makeSkippedKeyId(dhPublic, messageNumber) {
    const b64 = this.bytesToBase64(dhPublic);
    return `${b64}:${messageNumber}`;
  }
  /**
   * Convert Uint8Array to ArrayBuffer for Web Crypto compatibility.
   */
  toArrayBuffer(data) {
    const buffer = new ArrayBuffer(data.length);
    new Uint8Array(buffer).set(data);
    return buffer;
  }
  /**
   * XSalsa20-Poly1305 encryption (NaCl SecretBox).
   * Uses libsodium for compatibility with Python SDK.
   *
   * Note: XSalsa20-Poly1305 doesn't support AAD (Additional Authenticated Data).
   * The AAD parameter is kept for API compatibility but is not used.
   * Header authentication is implicit in the message structure.
   */
  async secretboxEncrypt(key, plaintext, _aad) {
    await ensureSodiumReady();
    const nonce = sodium__default.default.randombytes_buf(sodium__default.default.crypto_secretbox_NONCEBYTES);
    const encrypted = sodium__default.default.crypto_secretbox_easy(plaintext, nonce, key);
    const result = new Uint8Array(nonce.length + encrypted.length);
    result.set(nonce, 0);
    result.set(encrypted, nonce.length);
    return result;
  }
  /**
   * XSalsa20-Poly1305 decryption (NaCl SecretBox).
   * Uses libsodium for compatibility with Python SDK.
   */
  async secretboxDecrypt(key, ciphertext, _aad) {
    await ensureSodiumReady();
    const nonce = ciphertext.slice(0, sodium__default.default.crypto_secretbox_NONCEBYTES);
    const encrypted = ciphertext.slice(sodium__default.default.crypto_secretbox_NONCEBYTES);
    const plaintext = sodium__default.default.crypto_secretbox_open_easy(encrypted, nonce, key);
    if (!plaintext) {
      throw new Error("Decryption failed: authentication tag mismatch");
    }
    return plaintext;
  }
  // Aliases for backward compatibility (internal API)
  async aesEncrypt(key, plaintext, aad) {
    return this.secretboxEncrypt(key, plaintext, aad);
  }
  async aesDecrypt(key, ciphertext, aad) {
    return this.secretboxDecrypt(key, ciphertext, aad);
  }
  /**
   * Serialize header for AAD.
   */
  serializeHeader(header) {
    const data = {
      dh: this.bytesToBase64(header.dhPublicKey),
      pn: header.previousChainLength,
      n: header.messageNumber
    };
    return new TextEncoder().encode(JSON.stringify(data));
  }
  /**
   * Get the current state for persistence.
   */
  getState() {
    return { ...this.state };
  }
  /**
   * Serialize state for storage.
   */
  serializeState() {
    const skippedKeys = {};
    for (const [k, v] of this.state.skippedKeys) {
      skippedKeys[k] = this.bytesToBase64(v);
    }
    return {
      dh_private: this.bytesToBase64(this.state.dhPrivate),
      dh_public: this.bytesToBase64(this.state.dhPublic),
      peer_dh_public: this.state.peerDhPublic ? this.bytesToBase64(this.state.peerDhPublic) : null,
      root_key: this.bytesToBase64(this.state.rootKey),
      send_chain_key: this.state.sendChainKey ? this.bytesToBase64(this.state.sendChainKey) : null,
      recv_chain_key: this.state.recvChainKey ? this.bytesToBase64(this.state.recvChainKey) : null,
      send_message_number: this.state.sendMessageNumber,
      recv_message_number: this.state.recvMessageNumber,
      previous_chain_length: this.state.previousChainLength,
      skipped_keys: skippedKeys
    };
  }
  /**
   * Deserialize state from storage.
   */
  static deserializeState(data, isInitiator) {
    const fromBase642 = (b64) => {
      const binary = atob(b64);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) {
        bytes[i] = binary.charCodeAt(i);
      }
      return bytes;
    };
    const skippedKeys = /* @__PURE__ */ new Map();
    for (const [k, v] of Object.entries(data.skipped_keys)) {
      skippedKeys.set(k, fromBase642(v));
    }
    const state = {
      dhPrivate: fromBase642(data.dh_private),
      dhPublic: fromBase642(data.dh_public),
      peerDhPublic: data.peer_dh_public ? fromBase642(data.peer_dh_public) : null,
      rootKey: fromBase642(data.root_key),
      sendChainKey: data.send_chain_key ? fromBase642(data.send_chain_key) : null,
      recvChainKey: data.recv_chain_key ? fromBase642(data.recv_chain_key) : null,
      sendMessageNumber: data.send_message_number,
      recvMessageNumber: data.recv_message_number,
      previousChainLength: data.previous_chain_length,
      skippedKeys
    };
    return new _DoubleRatchetSession(state, isInitiator);
  }
  /**
   * Helper: bytes to base64.
   */
  bytesToBase64(bytes) {
    const binary = String.fromCharCode(...bytes);
    return btoa(binary);
  }
  /**
   * Helper: compare byte arrays.
   */
  bytesEqual(a, b) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      if (a[i] !== b[i]) return false;
    }
    return true;
  }
};
function serializeRatchetHeader(header) {
  const binary = String.fromCharCode(...header.dhPublicKey);
  return {
    dh_public_key: btoa(binary),
    pn: header.previousChainLength,
    n: header.messageNumber
  };
}
function deserializeRatchetHeader(data) {
  const binary = atob(data.dh_public_key);
  const dhPublicKey = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    dhPublicKey[i] = binary.charCodeAt(i);
  }
  return {
    dhPublicKey,
    previousChainLength: data.pn,
    messageNumber: data.n
  };
}

// src/encryption/session.ts
var SessionState = /* @__PURE__ */ ((SessionState2) => {
  SessionState2["PENDING"] = "pending";
  SessionState2["ACTIVE"] = "active";
  SessionState2["CLOSED"] = "closed";
  SessionState2["REJECTED"] = "rejected";
  return SessionState2;
})(SessionState || {});
var DEFAULT_SESSION_CONFIG = {
  ttl: 300,
  useDoubleRatchet: true,
  cleanupInterval: 6 * 60 * 60 * 1e3,
  // 6 hours
  staleThreshold: 7 * 24 * 60 * 60 * 1e3
  // 7 days
};
var SessionManager = class {
  identity;
  storage;
  config;
  prekeyManager;
  sessions = /* @__PURE__ */ new Map();
  cleanupTimer = null;
  constructor(identity, storage, prekeyManager, config = {}) {
    this.identity = identity;
    this.storage = storage;
    this.prekeyManager = prekeyManager;
    this.config = { ...DEFAULT_SESSION_CONFIG, ...config };
  }
  /**
   * Initialize session manager, loading persisted sessions.
   */
  async initialize() {
    const loaded = await this.loadAllSessions();
    this.cleanupTimer = setInterval(
      () => this.cleanupStaleSessions(),
      this.config.cleanupInterval
    );
    return loaded;
  }
  /**
   * Shutdown session manager.
   */
  async shutdown() {
    if (this.cleanupTimer) {
      clearInterval(this.cleanupTimer);
      this.cleanupTimer = null;
    }
    for (const [sessionId, session] of this.sessions) {
      await this.saveSession(sessionId, session.info, session.ratchet);
    }
  }
  /**
   * Initiate a new encrypted session with a peer.
   *
   * @param peerAmid - The peer's AgentMesh ID
   * @param peerBundle - The peer's prekey bundle
   * @param peerSigningKey - The peer's signing public key
   * @returns Session ID and initial X3DH message to send
   */
  async initiateSession(peerAmid, peerBundle, peerSigningKey) {
    const existing = this.getSessionByPeer(peerAmid);
    if (existing && existing.state === "active" /* ACTIVE */) {
      throw new Error(`Active session already exists with ${peerAmid}`);
    }
    const x3dhResult = await X3DHKeyExchange.initiator(
      this.identity,
      peerBundle,
      peerSigningKey
    );
    const sessionId = await this.generateSessionId();
    const ratchet = await DoubleRatchetSession.initialize(
      x3dhResult.sharedSecret,
      true,
      // is initiator
      peerBundle.signedPrekey
      // peer's signed prekey = initial ratchet key
    );
    const info = {
      sessionId,
      peerAmid,
      state: "pending" /* PENDING */,
      createdAt: /* @__PURE__ */ new Date(),
      lastUsed: /* @__PURE__ */ new Date(),
      isInitiator: true,
      messagesSent: 0,
      messagesReceived: 0
    };
    this.sessions.set(sessionId, { info, ratchet });
    await this.saveSession(sessionId, info, ratchet);
    return { sessionId, x3dhMessage: x3dhResult.initiatorMessage };
  }
  /**
   * Accept an incoming session from a peer.
   *
   * @param peerAmid - The peer's AgentMesh ID
   * @param x3dhMessage - The X3DH message from the initiator
   * @returns Session ID
   */
  async acceptSession(peerAmid, x3dhMessage) {
    const signedPrekeyPrivate = this.prekeyManager.getSignedPrekeyPrivate(x3dhMessage.signedPrekeyId);
    if (!signedPrekeyPrivate) {
      throw new Error(`Signed prekey ${x3dhMessage.signedPrekeyId} not found`);
    }
    let oneTimePrekeyPrivate = null;
    if (x3dhMessage.oneTimePrekeyId !== void 0) {
      oneTimePrekeyPrivate = this.prekeyManager.getOneTimePrekeyPrivate(x3dhMessage.oneTimePrekeyId);
      if (!oneTimePrekeyPrivate) {
        throw new Error(`One-time prekey ${x3dhMessage.oneTimePrekeyId} not found`);
      }
      await this.prekeyManager.consumePrekey(x3dhMessage.oneTimePrekeyId);
    }
    const x3dhResult = await X3DHKeyExchange.responder(
      this.identity,
      signedPrekeyPrivate,
      oneTimePrekeyPrivate,
      x3dhMessage
    );
    const sessionId = await this.generateSessionId();
    const signedPrekeyPublic = this.prekeyManager.getSignedPrekeyPublic(x3dhMessage.signedPrekeyId);
    const ratchet = await DoubleRatchetSession.initializeResponder(
      x3dhResult.sharedSecret,
      signedPrekeyPrivate,
      signedPrekeyPublic
    );
    const info = {
      sessionId,
      peerAmid,
      state: "active" /* ACTIVE */,
      createdAt: /* @__PURE__ */ new Date(),
      lastUsed: /* @__PURE__ */ new Date(),
      isInitiator: false,
      messagesSent: 0,
      messagesReceived: 0
    };
    this.sessions.set(sessionId, { info, ratchet });
    await this.saveSession(sessionId, info, ratchet);
    return sessionId;
  }
  /**
   * Activate a pending session (after receiving ACK from responder).
   */
  async activateSession(sessionId, peerRatchetKey) {
    const session = this.sessions.get(sessionId);
    if (!session) {
      throw new Error(`Session not found: ${sessionId}`);
    }
    if (session.info.state !== "pending" /* PENDING */) {
      throw new Error(`Session ${sessionId} is not pending`);
    }
    await session.ratchet.initializeReceiving(peerRatchetKey);
    session.info.state = "active" /* ACTIVE */;
    session.info.lastUsed = /* @__PURE__ */ new Date();
    await this.saveSession(sessionId, session.info, session.ratchet);
  }
  /**
   * Activate a session directly (peer DH key already set during initialization).
   */
  async activateSessionDirect(sessionId) {
    const session = this.sessions.get(sessionId);
    if (!session) {
      throw new Error(`Session not found: ${sessionId}`);
    }
    session.info.state = "active" /* ACTIVE */;
    session.info.lastUsed = /* @__PURE__ */ new Date();
    await this.saveSession(sessionId, session.info, session.ratchet);
  }
  /**
   * Encrypt a message for a session.
   */
  async encryptMessage(sessionId, plaintext) {
    const session = this.sessions.get(sessionId);
    if (!session) {
      throw new Error(`Session not found: ${sessionId}`);
    }
    if (session.info.state !== "active" /* ACTIVE */) {
      throw new Error(`Session ${sessionId} is not active`);
    }
    const plaintextBytes = new TextEncoder().encode(JSON.stringify(plaintext));
    const encrypted = await session.ratchet.encrypt(plaintextBytes);
    session.info.messagesSent++;
    session.info.lastUsed = /* @__PURE__ */ new Date();
    await this.saveSession(sessionId, session.info, session.ratchet);
    const header = serializeRatchetHeader(encrypted.header);
    const ciphertext = this.bytesToBase64(encrypted.ciphertext);
    return {
      session_id: sessionId,
      type: "encrypted",
      header,
      ciphertext
    };
  }
  /**
   * Decrypt a message from a session.
   */
  async decryptMessage(sessionId, envelope) {
    const session = this.sessions.get(sessionId);
    if (!session) {
      throw new Error(`Session not found: ${sessionId}`);
    }
    if (session.info.state !== "active" /* ACTIVE */) {
      throw new Error(`Session ${sessionId} is not active`);
    }
    const header = deserializeRatchetHeader(envelope.header);
    const ciphertext = this.base64ToBytes(envelope.ciphertext);
    const plaintextBytes = await session.ratchet.decrypt({ header, ciphertext });
    session.info.messagesReceived++;
    session.info.lastUsed = /* @__PURE__ */ new Date();
    await this.saveSession(sessionId, session.info, session.ratchet);
    return JSON.parse(new TextDecoder().decode(plaintextBytes));
  }
  /**
   * Close a session.
   */
  async closeSession(sessionId, reason = "normal") {
    const session = this.sessions.get(sessionId);
    if (!session) return;
    session.info.state = "closed" /* CLOSED */;
    await this.saveSession(sessionId, session.info, session.ratchet);
  }
  /**
   * Get session info.
   */
  getSession(sessionId) {
    return this.sessions.get(sessionId)?.info ?? null;
  }
  /**
   * Get session info (alias for getSession).
   */
  getSessionInfo(sessionId) {
    return this.getSession(sessionId);
  }
  /**
   * Get the ratchet public key for a session.
   * Used by initiators to complete session activation after responder accepts.
   */
  getRatchetPublicKey(sessionId) {
    const session = this.sessions.get(sessionId);
    if (!session) return null;
    return session.ratchet.getRatchetPublicKey();
  }
  /**
   * Get session by peer AMID.
   */
  getSessionByPeer(peerAmid) {
    for (const session of this.sessions.values()) {
      if (session.info.peerAmid === peerAmid) {
        return session.info;
      }
    }
    return null;
  }
  /**
   * List all sessions.
   */
  listSessions() {
    return Array.from(this.sessions.values()).map((s) => s.info);
  }
  /**
   * Get session count by state.
   */
  getSessionStats() {
    let active = 0, pending = 0, closed = 0;
    for (const session of this.sessions.values()) {
      switch (session.info.state) {
        case "active" /* ACTIVE */:
          active++;
          break;
        case "pending" /* PENDING */:
          pending++;
          break;
        case "closed" /* CLOSED */:
        case "rejected" /* REJECTED */:
          closed++;
          break;
      }
    }
    return { total: this.sessions.size, active, pending, closed };
  }
  /**
   * Generate unique session ID.
   */
  async generateSessionId() {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    const hex = Array.from(bytes).map((b) => b.toString(16).padStart(2, "0")).join("");
    return `session_${hex}`;
  }
  /**
   * Save session to storage.
   */
  async saveSession(sessionId, info, ratchet) {
    const data = {
      session_id: info.sessionId,
      peer_amid: info.peerAmid,
      state: info.state,
      created_at: info.createdAt.toISOString(),
      last_used: info.lastUsed.toISOString(),
      is_initiator: info.isInitiator,
      messages_sent: info.messagesSent,
      messages_received: info.messagesReceived,
      ratchet_state: ratchet.serializeState()
    };
    const path = `sessions/${sessionId}.json`;
    const bytes = new TextEncoder().encode(JSON.stringify(data, null, 2));
    await this.storage.set(path, bytes);
  }
  /**
   * Load session from storage.
   */
  async loadSession(sessionId) {
    const path = `sessions/${sessionId}.json`;
    const bytes = await this.storage.get(path);
    if (!bytes) return false;
    try {
      const data = JSON.parse(new TextDecoder().decode(bytes));
      const info = {
        sessionId: data.session_id,
        peerAmid: data.peer_amid,
        state: data.state,
        createdAt: new Date(data.created_at),
        lastUsed: new Date(data.last_used),
        isInitiator: data.is_initiator,
        messagesSent: data.messages_sent,
        messagesReceived: data.messages_received
      };
      const ratchet = DoubleRatchetSession.deserializeState(
        data.ratchet_state,
        info.isInitiator
      );
      this.sessions.set(sessionId, { info, ratchet });
      return true;
    } catch {
      return false;
    }
  }
  /**
   * Load all persisted sessions.
   */
  async loadAllSessions() {
    const files = await this.storage.list("sessions/");
    let loaded = 0;
    for (const file of files) {
      if (file.endsWith(".json")) {
        const sessionId = file.replace("sessions/", "").replace(".json", "");
        if (await this.loadSession(sessionId)) {
          loaded++;
        }
      }
    }
    return loaded;
  }
  /**
   * Clean up stale sessions.
   */
  async cleanupStaleSessions() {
    const now = Date.now();
    const staleIds = [];
    for (const [sessionId, session] of this.sessions) {
      const age = now - session.info.lastUsed.getTime();
      if (age > this.config.staleThreshold && session.info.state !== "active" /* ACTIVE */) {
        staleIds.push(sessionId);
      }
    }
    for (const sessionId of staleIds) {
      this.sessions.delete(sessionId);
      const path = `sessions/${sessionId}.json`;
      await this.storage.delete(path);
    }
    return staleIds.length;
  }
  /**
   * Helper: bytes to base64.
   */
  bytesToBase64(bytes) {
    const binary = String.fromCharCode(...bytes);
    return btoa(binary);
  }
  /**
   * Helper: base64 to bytes.
   */
  base64ToBytes(b64) {
    const binary = atob(b64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  }
};
var SessionCache = class {
  cache = /* @__PURE__ */ new Map();
  maxSize;
  defaultTtlMs;
  slidingWindowMs;
  stats = {
    hits: 0,
    misses: 0,
    evictions: 0,
    size: 0,
    maxSize: 0
  };
  eventHandlers = [];
  constructor(config = {}) {
    this.maxSize = config.maxCachedSessions ?? 1e3;
    this.defaultTtlMs = config.defaultTtlMs ?? 36e5;
    this.slidingWindowMs = config.slidingWindowMs ?? 3e5;
    this.stats.maxSize = this.maxSize;
  }
  /**
   * Generate a cache key from initiator, receiver, and intent.
   */
  generateKey(initiatorAmid, receiverAmid, intent) {
    const intentHash = this.hashIntent(intent);
    return `${initiatorAmid}:${receiverAmid}:${intentHash}`;
  }
  /**
   * Hash an intent string to a fixed-size key.
   */
  hashIntent(intent) {
    if (typeof window !== "undefined") {
      let hash = 0;
      for (let i = 0; i < intent.length; i++) {
        const char = intent.charCodeAt(i);
        hash = (hash << 5) - hash + char;
        hash = hash & hash;
      }
      return Math.abs(hash).toString(16).padStart(8, "0");
    } else {
      return crypto$1.createHash("sha256").update(intent).digest("hex").substring(0, 16);
    }
  }
  /**
   * Get a cached session if it exists and hasn't expired.
   */
  get(initiatorAmid, receiverAmid, intent) {
    const key = this.generateKey(initiatorAmid, receiverAmid, intent);
    const entry = this.cache.get(key);
    if (!entry) {
      this.stats.misses++;
      return null;
    }
    const now = Date.now();
    if (now > entry.expiresAt) {
      this.cache.delete(key);
      this.stats.size = this.cache.size;
      this.stats.misses++;
      return null;
    }
    this.cache.delete(key);
    entry.lastUsedAt = now;
    entry.usageCount++;
    entry.expiresAt = Math.min(
      entry.expiresAt + this.slidingWindowMs,
      entry.createdAt + this.defaultTtlMs * 2
      // Max extension
    );
    this.cache.set(key, entry);
    this.stats.hits++;
    return entry;
  }
  /**
   * Check if a session is cached without updating LRU order.
   */
  has(initiatorAmid, receiverAmid, intent) {
    const key = this.generateKey(initiatorAmid, receiverAmid, intent);
    const entry = this.cache.get(key);
    if (!entry) return false;
    if (Date.now() > entry.expiresAt) {
      this.cache.delete(key);
      this.stats.size = this.cache.size;
      return false;
    }
    return true;
  }
  /**
   * Cache a session.
   */
  set(sessionId, initiatorAmid, receiverAmid, intent, ttlMs) {
    const key = this.generateKey(initiatorAmid, receiverAmid, intent);
    const now = Date.now();
    const ttl = ttlMs ?? this.defaultTtlMs;
    if (this.cache.size >= this.maxSize && !this.cache.has(key)) {
      this.evictLRU();
    }
    const entry = {
      sessionId,
      initiatorAmid,
      receiverAmid,
      intentHash: this.hashIntent(intent),
      createdAt: now,
      lastUsedAt: now,
      expiresAt: now + ttl,
      usageCount: 0
    };
    this.cache.set(key, entry);
    this.stats.size = this.cache.size;
  }
  /**
   * Evict the least recently used entry.
   */
  evictLRU() {
    const firstKey = this.cache.keys().next().value;
    if (firstKey) {
      const evicted = this.cache.get(firstKey);
      this.cache.delete(firstKey);
      this.stats.evictions++;
      this.stats.size = this.cache.size;
      this.emitEvent("eviction", { key: firstKey, session: evicted });
    }
  }
  /**
   * Clear a specific cached session.
   */
  clear(initiatorAmid, receiverAmid, intent) {
    const key = this.generateKey(initiatorAmid, receiverAmid, intent);
    const existed = this.cache.delete(key);
    this.stats.size = this.cache.size;
    return existed;
  }
  /**
   * Clear a session by AMID (clears all sessions with that peer).
   */
  clearByAmid(amid) {
    let cleared = 0;
    for (const [key, entry] of this.cache.entries()) {
      if (entry.initiatorAmid === amid || entry.receiverAmid === amid) {
        this.cache.delete(key);
        cleared++;
      }
    }
    this.stats.size = this.cache.size;
    return cleared;
  }
  /**
   * Clear all cached sessions.
   */
  clearAll() {
    const previousSize = this.cache.size;
    this.cache.clear();
    this.stats.size = 0;
    this.emitEvent("cache_cleared", { clearedCount: previousSize });
  }
  /**
   * Get cache statistics.
   */
  getStats() {
    return { ...this.stats };
  }
  /**
   * Get all cached sessions (for debugging/dashboard).
   */
  getAll() {
    const now = Date.now();
    const sessions = [];
    for (const entry of this.cache.values()) {
      if (now <= entry.expiresAt) {
        sessions.push({ ...entry });
      }
    }
    return sessions;
  }
  /**
   * Clean up expired sessions.
   */
  cleanup() {
    const now = Date.now();
    let cleaned = 0;
    for (const [key, entry] of this.cache.entries()) {
      if (now > entry.expiresAt) {
        this.cache.delete(key);
        cleaned++;
      }
    }
    this.stats.size = this.cache.size;
    return cleaned;
  }
  /**
   * Register an event handler.
   */
  onEvent(handler) {
    this.eventHandlers.push(handler);
  }
  /**
   * Emit an event to all handlers.
   */
  emitEvent(type, data) {
    for (const handler of this.eventHandlers) {
      try {
        handler({ type, data });
      } catch {
      }
    }
  }
};

// src/session/index.ts
var SessionStateType = /* @__PURE__ */ ((SessionStateType2) => {
  SessionStateType2["PENDING"] = "PENDING";
  SessionStateType2["ACTIVE"] = "ACTIVE";
  SessionStateType2["CLOSED"] = "CLOSED";
  SessionStateType2["REJECTED"] = "REJECTED";
  SessionStateType2["EXPIRED"] = "EXPIRED";
  return SessionStateType2;
})(SessionStateType || {});
function generateNonce() {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return Array.from(bytes).map((b) => b.toString(16).padStart(2, "0")).join("");
}
function generateSessionId() {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return "sess_" + Array.from(bytes).map((b) => b.toString(16).padStart(2, "0")).join("");
}
var KnockProtocol = class {
  identity;
  policy;
  certManager;
  seenNonces = /* @__PURE__ */ new Set();
  nonceExpiryMs = 5 * 60 * 1e3;
  // 5 minutes
  constructor(identity, options) {
    this.identity = identity;
    this.policy = options?.policy;
    this.certManager = options?.certManager;
    if (options?.nonceExpiryMs) {
      this.nonceExpiryMs = options.nonceExpiryMs;
    }
  }
  /**
   * Set the policy for KNOCK evaluation.
   */
  setPolicy(policy) {
    this.policy = policy;
  }
  /**
   * Create a KNOCK message for session initiation.
   */
  async createKnock(toAmid, request, certificateChain) {
    const now = Date.now();
    const nonce = generateNonce();
    const messageData = {
      version: "agentmesh/0.2",
      from: this.identity.amid,
      to: toAmid,
      request,
      timestamp: now,
      nonce
    };
    const messageBytes = new TextEncoder().encode(JSON.stringify(messageData));
    const signature = await this.identity.sign(messageBytes);
    const knock = {
      ...messageData,
      signature: this.base64Encode(signature)
    };
    if (certificateChain && certificateChain.length > 0) {
      knock.certificateChain = certificateChain.map(
        (c) => this.base64Encode(c.raw)
      );
    }
    return knock;
  }
  /**
   * Validate and process an incoming KNOCK message.
   */
  async validateKnock(knock) {
    if (!knock.version.startsWith("agentmesh/")) {
      return { valid: false, error: "Unknown protocol version" };
    }
    if (knock.to !== this.identity.amid) {
      return { valid: false, error: "Message not addressed to us" };
    }
    const now = Date.now();
    const maxAge = this.nonceExpiryMs;
    if (Math.abs(now - knock.timestamp) > maxAge) {
      return { valid: false, error: "Message timestamp too old or in future" };
    }
    if (this.seenNonces.has(knock.nonce)) {
      return { valid: false, error: "Replay attack detected: duplicate nonce" };
    }
    this.base64Decode(knock.signature);
    const messageData = {
      version: knock.version,
      from: knock.from,
      to: knock.to,
      request: knock.request,
      timestamp: knock.timestamp,
      nonce: knock.nonce
    };
    new TextEncoder().encode(JSON.stringify(messageData));
    this.seenNonces.add(knock.nonce);
    setTimeout(() => {
      this.seenNonces.delete(knock.nonce);
    }, this.nonceExpiryMs);
    return { valid: true };
  }
  /**
   * Evaluate a KNOCK request against the policy.
   */
  async evaluateKnock(knock, senderInfo) {
    if (!this.policy) {
      return { allowed: true };
    }
    const context = {
      fromAmid: knock.from,
      fromTier: senderInfo?.tier || "anonymous",
      fromReputation: senderInfo?.reputation ?? 0,
      intentCategory: knock.request.intent.capability,
      requestedTtl: knock.request.ttl || 300
    };
    return this.policy.evaluate(context);
  }
  /**
   * Create an ACCEPT response.
   */
  async createAcceptResponse(knock, sessionId) {
    const id = sessionId || generateSessionId();
    const now = Date.now();
    const responseData = {
      type: "ACCEPT",
      sessionId: id,
      timestamp: now,
      from: this.identity.amid,
      to: knock.from,
      knockNonce: knock.nonce
    };
    const messageBytes = new TextEncoder().encode(JSON.stringify(responseData));
    const signature = await this.identity.sign(messageBytes);
    return {
      ...responseData,
      signature: this.base64Encode(signature)
    };
  }
  /**
   * Create a REJECT response.
   */
  async createRejectResponse(knock, reason) {
    const now = Date.now();
    const responseData = {
      type: "REJECT",
      reason,
      timestamp: now,
      from: this.identity.amid,
      to: knock.from,
      knockNonce: knock.nonce
    };
    const messageBytes = new TextEncoder().encode(JSON.stringify(responseData));
    const signature = await this.identity.sign(messageBytes);
    return {
      ...responseData,
      signature: this.base64Encode(signature)
    };
  }
  /**
   * Validate a KNOCK response.
   */
  async validateResponse(response, originalKnock) {
    if (response.knockNonce !== originalKnock.nonce) {
      return { valid: false, error: "Response nonce does not match KNOCK" };
    }
    if (response.from !== originalKnock.to || response.to !== originalKnock.from) {
      return { valid: false, error: "Response addresses do not match KNOCK" };
    }
    const now = Date.now();
    if (Math.abs(now - response.timestamp) > this.nonceExpiryMs) {
      return { valid: false, error: "Response timestamp too old" };
    }
    return { valid: true };
  }
  /**
   * Base64 encode bytes.
   */
  base64Encode(bytes) {
    const binary = String.fromCharCode(...bytes);
    return btoa(binary);
  }
  /**
   * Base64 decode to bytes.
   */
  base64Decode(b64) {
    let raw = b64;
    if (raw.startsWith("ed25519:")) raw = raw.slice(8);
    else if (raw.startsWith("x25519:")) raw = raw.slice(7);
    const binary = atob(raw);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  }
};
var ProtocolSessionManager = class {
  sessions = /* @__PURE__ */ new Map();
  sessionsByPeer = /* @__PURE__ */ new Map();
  /**
   * Create a new session.
   */
  createSession(remoteAmid, request, isInitiator, sessionId) {
    const id = sessionId || generateSessionId();
    const now = /* @__PURE__ */ new Date();
    const expiresAt = new Date(now.getTime() + request.ttl * 1e3);
    const session = {
      id,
      remoteAmid,
      state: "ACTIVE" /* ACTIVE */,
      request,
      createdAt: now,
      expiresAt,
      messagesSent: 0,
      messagesReceived: 0,
      lastActivity: now,
      isInitiator
    };
    this.sessions.set(id, session);
    if (!this.sessionsByPeer.has(remoteAmid)) {
      this.sessionsByPeer.set(remoteAmid, /* @__PURE__ */ new Set());
    }
    this.sessionsByPeer.get(remoteAmid).add(id);
    return session;
  }
  /**
   * Get a session by ID.
   */
  getSession(sessionId) {
    return this.sessions.get(sessionId);
  }
  /**
   * Get sessions for a peer.
   */
  getSessionsForPeer(remoteAmid) {
    const sessionIds = this.sessionsByPeer.get(remoteAmid);
    if (!sessionIds) return [];
    return Array.from(sessionIds).map((id) => this.sessions.get(id)).filter((s) => s !== void 0);
  }
  /**
   * Get all active sessions.
   */
  getActiveSessions() {
    return Array.from(this.sessions.values()).filter((s) => s.state === "ACTIVE" /* ACTIVE */);
  }
  /**
   * Update session state.
   */
  updateSessionState(sessionId, state) {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.state = state;
      session.lastActivity = /* @__PURE__ */ new Date();
    }
  }
  /**
   * Record a sent message.
   */
  recordMessageSent(sessionId) {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.messagesSent++;
      session.lastActivity = /* @__PURE__ */ new Date();
      if (session.request.expectedMessages && session.messagesSent >= session.request.expectedMessages) {
        if (session.request.type === "one-shot") {
          session.state = "CLOSED" /* CLOSED */;
        }
      }
    }
  }
  /**
   * Record a received message.
   */
  recordMessageReceived(sessionId) {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.messagesReceived++;
      session.lastActivity = /* @__PURE__ */ new Date();
    }
  }
  /**
   * Close a session.
   */
  closeSession(sessionId) {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.state = "CLOSED" /* CLOSED */;
      const peerSessions = this.sessionsByPeer.get(session.remoteAmid);
      if (peerSessions) {
        peerSessions.delete(sessionId);
        if (peerSessions.size === 0) {
          this.sessionsByPeer.delete(session.remoteAmid);
        }
      }
    }
  }
  /**
   * Clean up expired sessions.
   */
  cleanupExpiredSessions() {
    const now = /* @__PURE__ */ new Date();
    const expired = [];
    for (const session of this.sessions.values()) {
      if (session.expiresAt < now && session.state === "ACTIVE" /* ACTIVE */) {
        session.state = "EXPIRED" /* EXPIRED */;
        expired.push(session);
        const peerSessions = this.sessionsByPeer.get(session.remoteAmid);
        if (peerSessions) {
          peerSessions.delete(session.id);
          if (peerSessions.size === 0) {
            this.sessionsByPeer.delete(session.remoteAmid);
          }
        }
      }
    }
    return expired;
  }
  /**
   * Get session statistics.
   */
  getStats() {
    let active = 0, closed = 0, expired = 0, rejected = 0;
    for (const session of this.sessions.values()) {
      switch (session.state) {
        case "ACTIVE" /* ACTIVE */:
          active++;
          break;
        case "CLOSED" /* CLOSED */:
          closed++;
          break;
        case "EXPIRED" /* EXPIRED */:
          expired++;
          break;
        case "REJECTED" /* REJECTED */:
          rejected++;
          break;
      }
    }
    return {
      total: this.sessions.size,
      active,
      closed,
      expired,
      rejected
    };
  }
  /**
   * Clear all sessions.
   */
  clear() {
    this.sessions.clear();
    this.sessionsByPeer.clear();
  }
};
function serializeIntentToJSON(intent) {
  return {
    capability: intent.capability,
    action: intent.action,
    params: intent.params
  };
}
function deserializeIntentFromJSON(data) {
  return {
    capability: data.capability,
    action: data.action,
    params: data.params
  };
}

// src/audit/encrypted.ts
function toArrayBuffer2(data) {
  const buffer = new ArrayBuffer(data.length);
  new Uint8Array(buffer).set(data);
  return buffer;
}
function toBase64(data) {
  let binary = "";
  for (let i = 0; i < data.length; i++) {
    binary += String.fromCharCode(data[i]);
  }
  return btoa(binary);
}
function fromBase64(b64) {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}
var EncryptedAuditLogger = class {
  baseLogger;
  encryptionKey = null;
  encrypted;
  encryptedEntries = [];
  keyInitialized = false;
  constructor(config) {
    this.baseLogger = new AuditLogger(config);
    this.encrypted = config.encrypted ?? true;
    if (!this.encrypted) {
      console.warn("Audit encryption disabled. Logs will be stored in plaintext.");
    }
  }
  /**
   * Initialize encryption key from identity.
   * Must be called before logging if encryption is enabled.
   */
  async initializeKey(identity) {
    if (!this.encrypted) {
      this.keyInitialized = true;
      return;
    }
    const seed = await identity.deriveSecret("agentmesh_audit_key");
    this.encryptionKey = await hkdfSimple(seed, "audit_encryption_key", 32);
    this.keyInitialized = true;
  }
  /**
   * Check if key is initialized.
   */
  get isKeyInitialized() {
    return this.keyInitialized;
  }
  /**
   * Log an audit event.
   */
  async log(type, severity, message, options) {
    return this.baseLogger.log(type, severity, message, options);
  }
  /**
   * Query audit events.
   */
  query(options) {
    return this.baseLogger.query(options);
  }
  /**
   * Get event count.
   */
  getCount() {
    return this.baseLogger.getCount();
  }
  /**
   * Get recent events.
   */
  getRecent(count) {
    return this.baseLogger.getRecent(count);
  }
  /**
   * Clear all events.
   */
  clear() {
    this.baseLogger.clear();
    this.encryptedEntries = [];
  }
  /**
   * Export plaintext audit log.
   */
  export() {
    return this.baseLogger.export();
  }
  /**
   * Serialize event for encryption.
   */
  serializeEvent(event) {
    return {
      id: event.id,
      type: event.type,
      severity: event.severity,
      timestamp: event.timestamp.toISOString(),
      amid: event.amid,
      peerAmid: event.peerAmid,
      sessionId: event.sessionId,
      message: event.message,
      metadata: event.metadata,
      error: event.error
    };
  }
  /**
   * Deserialize event from decrypted data.
   */
  deserializeEvent(data) {
    return {
      id: data.id,
      type: data.type,
      severity: data.severity,
      timestamp: new Date(data.timestamp),
      amid: data.amid,
      peerAmid: data.peerAmid,
      sessionId: data.sessionId,
      message: data.message,
      metadata: data.metadata,
      error: data.error
    };
  }
  /**
   * Encrypt an audit entry.
   */
  async encryptEntry(event) {
    if (!this.encrypted || !this.encryptionKey) {
      throw new Error("Encryption not initialized");
    }
    const plaintext = new TextEncoder().encode(JSON.stringify(this.serializeEvent(event)));
    const aesKey = await crypto.subtle.importKey(
      "raw",
      toArrayBuffer2(this.encryptionKey),
      { name: "AES-GCM" },
      false,
      ["encrypt"]
    );
    const nonce = crypto.getRandomValues(new Uint8Array(12));
    const ciphertext = await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: toArrayBuffer2(nonce) },
      aesKey,
      toArrayBuffer2(plaintext)
    );
    return {
      nonce: toBase64(nonce),
      ciphertext: toBase64(new Uint8Array(ciphertext)),
      id: event.id,
      timestamp: event.timestamp.toISOString()
    };
  }
  /**
   * Decrypt an audit entry.
   */
  async decryptEntry(entry) {
    if (!this.encrypted || !this.encryptionKey) {
      throw new Error("Encryption not initialized");
    }
    const nonce = fromBase64(entry.nonce);
    const ciphertext = fromBase64(entry.ciphertext);
    const aesKey = await crypto.subtle.importKey(
      "raw",
      toArrayBuffer2(this.encryptionKey),
      { name: "AES-GCM" },
      false,
      ["decrypt"]
    );
    const plaintext = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: toArrayBuffer2(nonce) },
      aesKey,
      toArrayBuffer2(ciphertext)
    );
    const data = JSON.parse(new TextDecoder().decode(plaintext));
    return this.deserializeEvent(data);
  }
  /**
   * Export encrypted audit log.
   * Returns entries filtered by options, encrypted if encryption is enabled.
   */
  async exportAuditLog(identity, options) {
    if (!this.keyInitialized) {
      await this.initializeKey(identity);
    }
    const events = this.query(options);
    if (!this.encrypted) {
      const entries2 = events.map((event) => ({
        nonce: "",
        ciphertext: toBase64(new TextEncoder().encode(JSON.stringify(this.serializeEvent(event)))),
        id: event.id,
        timestamp: event.timestamp.toISOString()
      }));
      return { entries: entries2, encrypted: false };
    }
    const entries = [];
    for (const event of events) {
      entries.push(await this.encryptEntry(event));
    }
    return { entries, encrypted: true };
  }
  /**
   * Import and decrypt audit log.
   */
  async importAuditLog(identity, data) {
    if (!this.keyInitialized) {
      await this.initializeKey(identity);
    }
    let imported = 0;
    for (const entry of data.entries) {
      try {
        let event;
        if (data.encrypted && this.encrypted) {
          event = await this.decryptEntry(entry);
        } else {
          const plaintext = fromBase64(entry.ciphertext);
          const eventData = JSON.parse(new TextDecoder().decode(plaintext));
          event = this.deserializeEvent(eventData);
        }
        const json = JSON.stringify([this.serializeEvent(event)]);
        this.baseLogger.import(json);
        imported++;
      } catch {
      }
    }
    return imported;
  }
  /**
   * Re-encrypt transcripts with a new key.
   * Used for key rotation.
   */
  async reencryptTranscripts(oldIdentity, newIdentity, onProgress) {
    if (!this.encrypted) {
      return { reencrypted: 0, failed: 0 };
    }
    const oldSeed = await oldIdentity.deriveSecret("agentmesh_audit_key");
    const oldKey = await hkdfSimple(oldSeed, "audit_encryption_key", 32);
    const newSeed = await newIdentity.deriveSecret("agentmesh_audit_key");
    const newKey = await hkdfSimple(newSeed, "audit_encryption_key", 32);
    const entries = [...this.encryptedEntries];
    let reencrypted = 0;
    let failed = 0;
    for (let i = 0; i < entries.length; i++) {
      const entry = entries[i];
      if (!entry) continue;
      try {
        const nonce = fromBase64(entry.nonce);
        const ciphertext = fromBase64(entry.ciphertext);
        const oldAesKey = await crypto.subtle.importKey(
          "raw",
          toArrayBuffer2(oldKey),
          { name: "AES-GCM" },
          false,
          ["decrypt"]
        );
        const plaintext = await crypto.subtle.decrypt(
          { name: "AES-GCM", iv: toArrayBuffer2(nonce) },
          oldAesKey,
          toArrayBuffer2(ciphertext)
        );
        const newAesKey = await crypto.subtle.importKey(
          "raw",
          toArrayBuffer2(newKey),
          { name: "AES-GCM" },
          false,
          ["encrypt"]
        );
        const newNonce = crypto.getRandomValues(new Uint8Array(12));
        const newCiphertext = await crypto.subtle.encrypt(
          { name: "AES-GCM", iv: toArrayBuffer2(newNonce) },
          newAesKey,
          plaintext
        );
        entries[i] = {
          id: entry.id,
          timestamp: entry.timestamp,
          nonce: toBase64(newNonce),
          ciphertext: toBase64(new Uint8Array(newCiphertext))
        };
        reencrypted++;
        if (onProgress) {
          onProgress(i + 1, entries.length);
        }
      } catch {
        failed++;
      }
    }
    this.encryptedEntries = entries;
    this.encryptionKey = newKey;
    return { reencrypted, failed };
  }
  // Convenience methods delegated to base logger
  async logIdentityCreated(metadata) {
    return this.baseLogger.logIdentityCreated(metadata);
  }
  async logSessionInitiated(peerAmid, sessionId) {
    return this.baseLogger.logSessionInitiated(peerAmid, sessionId);
  }
  async logMessageSent(peerAmid, sessionId) {
    return this.baseLogger.logMessageSent(peerAmid, sessionId);
  }
  async logMessageReceived(peerAmid, sessionId) {
    return this.baseLogger.logMessageReceived(peerAmid, sessionId);
  }
  async logError(message, error, metadata) {
    return this.baseLogger.logError(message, error, metadata);
  }
  async logWarning(message, metadata) {
    return this.baseLogger.logWarning(message, metadata);
  }
};
function createEncryptedAuditLogger(amid, options) {
  return new EncryptedAuditLogger({
    amid,
    ...options
  });
}

// src/audit/index.ts
var SEVERITY_PRIORITY = {
  DEBUG: 0,
  INFO: 1,
  WARNING: 2,
  ERROR: 3,
  CRITICAL: 4
};
function generateEventId() {
  const timestamp = Date.now().toString(36);
  const random = Math.random().toString(36).slice(2, 10);
  return `audit_${timestamp}_${random}`;
}
var AuditLogger = class {
  config;
  events = [];
  minSeverityLevel;
  constructor(config) {
    this.config = {
      maxMemoryEvents: 1e3,
      minSeverity: "INFO",
      consoleOutput: false,
      storagePrefix: "audit",
      ...config
    };
    this.minSeverityLevel = SEVERITY_PRIORITY[this.config.minSeverity || "INFO"];
  }
  /**
   * Log an audit event.
   */
  async log(type, severity, message, options) {
    if (SEVERITY_PRIORITY[severity] < this.minSeverityLevel) {
      return this.createEvent(type, severity, message, options);
    }
    const event = this.createEvent(type, severity, message, options);
    this.events.push(event);
    if (this.events.length > (this.config.maxMemoryEvents || 1e3)) {
      this.events.shift();
    }
    if (this.config.storage) {
      await this.persistEvent(event);
    }
    if (this.config.consoleOutput) {
      this.logToConsole(event);
    }
    return event;
  }
  /**
   * Create an event object.
   */
  createEvent(type, severity, message, options) {
    const event = {
      id: generateEventId(),
      type,
      severity,
      timestamp: /* @__PURE__ */ new Date(),
      amid: this.config.amid,
      message
    };
    if (options?.peerAmid) {
      event.peerAmid = options.peerAmid;
    }
    if (options?.sessionId) {
      event.sessionId = options.sessionId;
    }
    if (options?.metadata) {
      event.metadata = options.metadata;
    }
    if (options?.error) {
      event.error = {
        name: options.error.name,
        message: options.error.message,
        stack: options.error.stack
      };
    }
    return event;
  }
  /**
   * Persist an event to storage.
   */
  async persistEvent(event) {
    if (!this.config.storage) return;
    const key = `${this.config.storagePrefix}/${event.id}`;
    const serialized = this.serializeEvent(event);
    await this.config.storage.set(key, new TextEncoder().encode(JSON.stringify(serialized)));
  }
  /**
   * Serialize an event for storage.
   */
  serializeEvent(event) {
    return {
      ...event,
      timestamp: event.timestamp.toISOString()
    };
  }
  /**
   * Deserialize an event from storage.
   */
  deserializeEvent(data) {
    return {
      ...data,
      type: data.type,
      severity: data.severity,
      timestamp: new Date(data.timestamp)
    };
  }
  /**
   * Log to console.
   */
  logToConsole(event) {
    const prefix = `[${event.severity}] [${event.type}]`;
    const msg = `${prefix} ${event.message}`;
    switch (event.severity) {
      case "DEBUG":
        console.debug(msg, event.metadata || "");
        break;
      case "INFO":
        console.info(msg, event.metadata || "");
        break;
      case "WARNING":
        console.warn(msg, event.metadata || "");
        break;
      case "ERROR":
      case "CRITICAL":
        console.error(msg, event.error || event.metadata || "");
        break;
    }
  }
  /**
   * Query audit events.
   */
  query(options = {}) {
    let results = [...this.events];
    if (options.type) {
      const types = Array.isArray(options.type) ? options.type : [options.type];
      results = results.filter((e) => types.includes(e.type));
    }
    if (options.severity) {
      const severities = Array.isArray(options.severity) ? options.severity : [options.severity];
      results = results.filter((e) => severities.includes(e.severity));
    }
    if (options.peerAmid) {
      results = results.filter((e) => e.peerAmid === options.peerAmid);
    }
    if (options.sessionId) {
      results = results.filter((e) => e.sessionId === options.sessionId);
    }
    if (options.startTime) {
      results = results.filter((e) => e.timestamp >= options.startTime);
    }
    if (options.endTime) {
      results = results.filter((e) => e.timestamp <= options.endTime);
    }
    const order = options.order || "desc";
    results.sort((a, b) => {
      const diff = a.timestamp.getTime() - b.timestamp.getTime();
      return order === "asc" ? diff : -diff;
    });
    const offset = options.offset || 0;
    const limit = options.limit || results.length;
    results = results.slice(offset, offset + limit);
    return results;
  }
  /**
   * Get events by type.
   */
  getByType(type) {
    return this.query({ type });
  }
  /**
   * Get events for a peer.
   */
  getByPeer(peerAmid) {
    return this.query({ peerAmid });
  }
  /**
   * Get events for a session.
   */
  getBySession(sessionId) {
    return this.query({ sessionId });
  }
  /**
   * Get error events.
   */
  getErrors() {
    return this.query({ severity: ["ERROR", "CRITICAL"] });
  }
  /**
   * Get recent events.
   */
  getRecent(count = 10) {
    return this.query({ limit: count, order: "desc" });
  }
  /**
   * Get event count.
   */
  getCount() {
    return this.events.length;
  }
  /**
   * Get event statistics.
   */
  getStats() {
    const byType = {};
    const bySeverity = {};
    for (const event of this.events) {
      byType[event.type] = (byType[event.type] || 0) + 1;
      bySeverity[event.severity] = (bySeverity[event.severity] || 0) + 1;
    }
    return {
      total: this.events.length,
      byType,
      bySeverity
    };
  }
  /**
   * Clear all events from memory.
   */
  clear() {
    this.events = [];
  }
  /**
   * Export all events to JSON.
   */
  export() {
    const serialized = this.events.map((e) => this.serializeEvent(e));
    return JSON.stringify(serialized, null, 2);
  }
  /**
   * Import events from JSON.
   */
  import(json) {
    const data = JSON.parse(json);
    let imported = 0;
    for (const item of data) {
      try {
        const event = this.deserializeEvent(item);
        this.events.push(event);
        imported++;
      } catch {
      }
    }
    this.events.sort((a, b) => a.timestamp.getTime() - b.timestamp.getTime());
    while (this.events.length > (this.config.maxMemoryEvents || 1e3)) {
      this.events.shift();
    }
    return imported;
  }
  // Convenience methods for common events
  /** Log identity creation */
  async logIdentityCreated(metadata) {
    return this.log("IDENTITY_CREATED", "INFO", "Identity created", { metadata });
  }
  /** Log session initiation */
  async logSessionInitiated(peerAmid, sessionId) {
    return this.log("SESSION_INITIATED", "INFO", `Session initiated with ${peerAmid}`, {
      peerAmid,
      sessionId
    });
  }
  /** Log message sent */
  async logMessageSent(peerAmid, sessionId) {
    return this.log("MESSAGE_SENT", "DEBUG", `Message sent to ${peerAmid}`, {
      peerAmid,
      sessionId
    });
  }
  /** Log message received */
  async logMessageReceived(peerAmid, sessionId) {
    return this.log("MESSAGE_RECEIVED", "DEBUG", `Message received from ${peerAmid}`, {
      peerAmid,
      sessionId
    });
  }
  /** Log error */
  async logError(message, error, metadata) {
    return this.log("ERROR", "ERROR", message, { error, metadata });
  }
  /** Log warning */
  async logWarning(message, metadata) {
    return this.log("WARNING", "WARNING", message, { metadata });
  }
};
function createAuditLogger(amid, options) {
  return new AuditLogger({
    amid,
    ...options
  });
}

// src/rate-limiter.ts
var RateLimitError = class extends Error {
  code = "RATE_LIMITED";
  retryAfter;
  constructor(retryAfterMs) {
    super(`Rate limit exceeded. Retry after ${retryAfterMs}ms`);
    this.name = "RateLimitError";
    this.retryAfter = retryAfterMs;
    Object.setPrototypeOf(this, new.target.prototype);
  }
};
var TokenBucket = class {
  tokens;
  lastRefill;
  maxTokens;
  refillRate;
  // tokens per millisecond
  constructor(maxTokens, tokensPerSecond) {
    this.maxTokens = maxTokens;
    this.tokens = maxTokens;
    this.refillRate = tokensPerSecond / 1e3;
    this.lastRefill = Date.now();
  }
  /**
   * Try to consume a token.
   * Returns true if successful, false if rate limited.
   */
  tryConsume() {
    this.refill();
    if (this.tokens >= 1) {
      this.tokens -= 1;
      return true;
    }
    return false;
  }
  /**
   * Get time until next token is available (in ms).
   */
  getRetryAfter() {
    this.refill();
    if (this.tokens >= 1) {
      return 0;
    }
    return Math.ceil((1 - this.tokens) / this.refillRate);
  }
  /**
   * Get current status.
   */
  getStatus() {
    this.refill();
    return {
      tokens: Math.floor(this.tokens),
      maxTokens: this.maxTokens
    };
  }
  /**
   * Refill tokens based on time elapsed.
   */
  refill() {
    const now = Date.now();
    const elapsed = now - this.lastRefill;
    const tokensToAdd = elapsed * this.refillRate;
    this.tokens = Math.min(this.maxTokens, this.tokens + tokensToAdd);
    this.lastRefill = now;
  }
};
var RateLimiter = class {
  globalBucket;
  peerBuckets = /* @__PURE__ */ new Map();
  peerMaxTokens;
  peerRefillRate;
  eventHandlers = [];
  constructor(config = {}) {
    const maxPerSecond = config.maxPerSecond ?? 100;
    const maxBurst = config.maxBurst ?? 500;
    this.globalBucket = new TokenBucket(maxBurst, maxPerSecond);
    this.peerMaxTokens = config.perPeer?.maxPerSecond ?? 50;
    this.peerRefillRate = config.perPeer?.maxPerSecond ?? 50;
  }
  /**
   * Check if a message can be sent (consume token).
   * @param peerAmid - Optional peer AMID for per-peer limiting
   * @throws RateLimitError if rate limit exceeded
   */
  consume(peerAmid) {
    if (!this.globalBucket.tryConsume()) {
      const retryAfter = this.globalBucket.getRetryAfter();
      this.emitEvent("rate_limited", {
        scope: "global",
        peerAmid,
        retryAfter
      });
      throw new RateLimitError(retryAfter);
    }
    if (peerAmid) {
      const peerBucket = this.getOrCreatePeerBucket(peerAmid);
      if (!peerBucket.tryConsume()) {
        const retryAfter = peerBucket.getRetryAfter();
        this.emitEvent("rate_limited", {
          scope: "peer",
          peerAmid,
          retryAfter
        });
        throw new RateLimitError(retryAfter);
      }
    }
  }
  /**
   * Check if a message can be sent without consuming.
   */
  canConsume(peerAmid) {
    const globalStatus = this.globalBucket.getStatus();
    if (globalStatus.tokens < 1) {
      return false;
    }
    if (peerAmid) {
      const peerBucket = this.getOrCreatePeerBucket(peerAmid);
      const peerStatus = peerBucket.getStatus();
      if (peerStatus.tokens < 1) {
        return false;
      }
    }
    return true;
  }
  /**
   * Get retry time if rate limited.
   */
  getRetryAfter(peerAmid) {
    const globalRetry = this.globalBucket.getRetryAfter();
    if (!peerAmid) {
      return globalRetry;
    }
    const peerBucket = this.getOrCreatePeerBucket(peerAmid);
    const peerRetry = peerBucket.getRetryAfter();
    return Math.max(globalRetry, peerRetry);
  }
  /**
   * Get current rate limit status.
   */
  getStatus() {
    const globalStatus = this.globalBucket.getStatus();
    const peerStatuses = /* @__PURE__ */ new Map();
    for (const [amid, bucket] of this.peerBuckets) {
      peerStatuses.set(amid, bucket.getStatus());
    }
    return {
      tokens: globalStatus.tokens,
      maxTokens: globalStatus.maxTokens,
      refillRate: this.peerRefillRate,
      peerStatuses
    };
  }
  /**
   * Register an event handler.
   */
  onEvent(handler) {
    this.eventHandlers.push(handler);
  }
  /**
   * Wait for capacity to become available.
   * @param maxWaitMs - Maximum time to wait
   * @param peerAmid - Optional peer for per-peer limit
   * @returns true if capacity available, false if timeout
   */
  async waitForCapacity(maxWaitMs, peerAmid) {
    const startTime = Date.now();
    while (Date.now() - startTime < maxWaitMs) {
      if (this.canConsume(peerAmid)) {
        return true;
      }
      const retryAfter = Math.min(
        this.getRetryAfter(peerAmid),
        maxWaitMs - (Date.now() - startTime)
      );
      if (retryAfter <= 0) {
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, retryAfter));
    }
    return this.canConsume(peerAmid);
  }
  /**
   * Get or create a peer bucket.
   */
  getOrCreatePeerBucket(peerAmid) {
    let bucket = this.peerBuckets.get(peerAmid);
    if (!bucket) {
      bucket = new TokenBucket(this.peerMaxTokens, this.peerRefillRate);
      this.peerBuckets.set(peerAmid, bucket);
    }
    return bucket;
  }
  /**
   * Emit an event to handlers.
   */
  emitEvent(type, data) {
    for (const handler of this.eventHandlers) {
      try {
        handler({ type, data });
      } catch {
      }
    }
  }
};

// src/client.ts
var CircuitState = /* @__PURE__ */ ((CircuitState2) => {
  CircuitState2["RUNNING"] = "RUNNING";
  CircuitState2["PAUSED"] = "PAUSED";
  CircuitState2["STOPPED"] = "STOPPED";
  return CircuitState2;
})(CircuitState || {});
var AgentMeshClient = class _AgentMeshClient {
  identity;
  registry;
  transport;
  sessionManager;
  prekeyManager;
  protocolSessions;
  knockProtocol;
  auditLogger;
  storage;
  registryUrl;
  relayUrl;
  capabilities = [];
  policy;
  connected = false;
  activeSessions = /* @__PURE__ */ new Map();
  // peerAmid -> sessionId
  messageHandlers = [];
  errorHandlers = [];
  knockHandler;
  e2eVerifiedPeers = /* @__PURE__ */ new Set();
  eventHandlers = /* @__PURE__ */ new Map();
  // Circuit breaker state
  circuitState = "RUNNING" /* RUNNING */;
  circuitStateChangedAt = Date.now();
  blocklist = /* @__PURE__ */ new Set();
  // Rate limiting
  rateLimiter;
  // Optimistic send
  optimisticSendEnabled = false;
  optimisticAllowlist = /* @__PURE__ */ new Set();
  // Session cache
  sessionCache;
  // Pending X3DH initiator messages (sent with first encrypted message to each peer)
  pendingX3DH = /* @__PURE__ */ new Map();
  constructor(identity, options = {}) {
    this.identity = identity;
    this.storage = options.storage || new chunkC7KJHFTP_cjs.MemoryStorage();
    this.registryUrl = options.registryUrl || "https://agentmesh.online/v1";
    this.relayUrl = options.relayUrl || "wss://relay.agentmesh.online/v1/connect";
    this.registry = new chunkUBUGIENK_cjs.RegistryClient(this.registryUrl);
    const transportOptions = {
      relayUrl: this.relayUrl
    };
    this.transport = new chunkFAEZQCEA_cjs.RelayTransport(identity, transportOptions);
    this.prekeyManager = new PrekeyManager(identity, this.storage);
    this.sessionManager = new SessionManager(identity, this.storage, this.prekeyManager, options.sessionConfig);
    this.protocolSessions = new ProtocolSessionManager();
    this.knockProtocol = new KnockProtocol(identity);
    this.auditLogger = createAuditLogger(identity.amid);
    if (options.rateLimit) {
      this.rateLimiter = new RateLimiter(options.rateLimit);
      this.rateLimiter.onEvent((event) => {
        if (event.type === "rate_limited") {
          this.emitEvent("rate_limited", event.data);
        }
      });
    }
    this.optimisticSendEnabled = options.optimisticSend ?? false;
    if (options.optimisticAllowlist) {
      for (const amid of options.optimisticAllowlist) {
        this.optimisticAllowlist.add(amid);
      }
    }
    this.sessionCache = new SessionCache(options.sessionCache);
    this.transport.onTransportEvent("optimistic_dropped", (data) => {
      this.emitEvent("optimistic_dropped", data);
    });
  }
  /**
   * Create a new client with a generated identity.
   */
  static async create(options) {
    const identity = await chunkBPYP43TA_cjs.Identity.generate();
    return new _AgentMeshClient(identity, options || {});
  }
  /**
   * Load a client from storage.
   */
  static async load(storage, path = "identity", options) {
    const identity = await chunkBPYP43TA_cjs.Identity.load(storage, path);
    return new _AgentMeshClient(identity, { storage, ...options });
  }
  /**
   * Create a client from an existing identity.
   */
  static fromIdentity(identity, options) {
    return new _AgentMeshClient(identity, options);
  }
  /**
   * Get the underlying identity.
   */
  getIdentity() {
    return this.identity;
  }
  /**
   * Get the client's AMID.
   */
  get amid() {
    return this.identity.amid;
  }
  /**
   * Connect to the AgentMesh network.
   */
  async connect(options = {}) {
    if (this.connected) {
      throw new chunkFNHOFD2H_cjs.AgentMeshError("Already connected", "ALREADY_CONNECTED");
    }
    this.capabilities = options.capabilities || [];
    this.policy = options.policy;
    if (this.policy) {
      this.knockProtocol.setPolicy(this.policy);
    }
    await this.prekeyManager.loadOrInitialize();
    const registerOptions = {
      displayName: options.displayName,
      capabilities: this.capabilities
    };
    await this.registry.register(this.identity, registerOptions);
    if (options.autoUploadPrekeys !== false) {
      await this.uploadPrekeys();
    }
    await this.transport.connect();
    this.transport.onMessage("receive", async (data) => {
      const fromAmid = data.from;
      const rawPayload = data.encrypted_payload;
      const msgType = data.message_type;
      try {
        const parsed = JSON.parse(rawPayload);
        if (msgType === "knock") {
          const request = parsed.request || parsed;
          const result = await this.handleIncomingKnock(fromAmid, request);
          if (result.accept) {
            try {
              const accept = await this.knockProtocol.createAcceptResponse(parsed, result.sessionId);
              await this.transport.send(fromAmid, JSON.stringify(accept), "accept");
            } catch {
            }
          }
        } else if (parsed.type === "encrypted" && parsed.x3dh) {
          console.log(`[AGT] Received encrypted+x3dh message from ${fromAmid}`);
          try {
            const x3dhMsg = deserializeX3DHMessage(parsed.x3dh);
            const sessionId = await this.sessionManager.acceptSession(fromAmid, x3dhMsg);
            this.activeSessions.set(fromAmid, sessionId);
            const decrypted = await this.sessionManager.decryptMessage(sessionId, parsed);
            this.emitE2EVerified(fromAmid);
            for (const handler of this.messageHandlers) {
              try {
                handler(fromAmid, decrypted);
              } catch {
              }
            }
          } catch (e) {
            console.error("[AGT] E2E decrypt failed \u2014 message REJECTED (not delivered):", e?.message || e);
            for (const handler of this.errorHandlers) {
              try {
                handler("decrypt_failed", fromAmid, e?.message || "unknown");
              } catch {
              }
            }
          }
        } else if (parsed.type === "encrypted") {
          console.log(`[AGT] Received encrypted message (no x3dh) from ${fromAmid}`);
          const sessionId = this.activeSessions.get(fromAmid);
          if (sessionId) {
            try {
              const decrypted = await this.sessionManager.decryptMessage(sessionId, parsed);
              this.emitE2EVerified(fromAmid);
              for (const handler of this.messageHandlers) {
                try {
                  handler(fromAmid, decrypted);
                } catch {
                }
              }
            } catch (decErr) {
              console.error("[AGT] E2E decrypt failed for existing session \u2014 message REJECTED:", decErr?.message || decErr);
              for (const handler of this.errorHandlers) {
                try {
                  handler("decrypt_failed", fromAmid, decErr?.message || "unknown");
                } catch {
                }
              }
            }
          } else {
            console.error(`[AGT] Encrypted message from ${fromAmid} but no session \u2014 message REJECTED`);
            for (const handler of this.errorHandlers) {
              try {
                handler("no_session", fromAmid, "No encryption session established");
              } catch {
              }
            }
          }
        } else {
          console.log(`[AGT] Received PLAIN message from ${fromAmid}, type=${parsed.type}, keys=${Object.keys(parsed).join(",")}`);
          for (const handler of this.messageHandlers) {
            try {
              handler(fromAmid, parsed);
            } catch {
            }
          }
        }
      } catch {
        for (const handler of this.messageHandlers) {
          try {
            handler(fromAmid, rawPayload);
          } catch {
          }
        }
      }
    });
    this.connected = true;
    this.emitEvent("connected", { amid: this.amid });
    await this.auditLogger.log("CONNECTION_ESTABLISHED", "INFO", "Connected to AgentMesh");
  }
  /**
   * Disconnect from the AgentMesh network.
   */
  async disconnect() {
    if (!this.connected) return;
    try {
      await this.registry.updateStatus(this.identity, "offline");
    } catch {
    }
    await this.transport.disconnect("Client disconnect");
    this.connected = false;
    this.emitEvent("disconnected", { amid: this.amid });
    await this.auditLogger.log("CONNECTION_LOST", "INFO", "Disconnected from AgentMesh");
  }
  /**
   * Check if connected.
   */
  get isConnected() {
    return this.connected && this.transport.isConnected;
  }
  /**
   * Search for agents with a capability.
   */
  async search(capability, options) {
    const result = await this.registry.search({
      capability,
      limit: options?.limit,
      tierMin: options?.tierMin
    });
    return result.results;
  }
  /**
   * Look up an agent by AMID.
   */
  async lookup(amid) {
    return this.registry.lookup(amid);
  }
  /**
   * Submit reputation feedback for a peer agent after an interaction.
   */
  async submitReputation(targetAmid, sessionId, score, tags) {
    return this.registry.submitReputation(
      this.identity,
      targetAmid,
      sessionId,
      score,
      tags
    );
  }
  /**
   * Send a message to an agent.
   */
  async send(toAmid, message, options = {}) {
    if (this.circuitState === "STOPPED" /* STOPPED */) {
      throw new chunkFNHOFD2H_cjs.AgentMeshError("Client is stopped", "CLIENT_STOPPED");
    }
    if (!this.connected) {
      throw new chunkFNHOFD2H_cjs.NetworkError("Not connected", "NOT_CONNECTED");
    }
    if (this.isBlocked(toAmid)) {
      throw new chunkFNHOFD2H_cjs.SessionError(`Peer ${toAmid} is blocked`, "PEER_BLOCKED");
    }
    if (this.rateLimiter) {
      this.rateLimiter.consume(toAmid);
    }
    let sessionId = this.activeSessions.get(toAmid);
    const intent = options.intent || "*";
    if (!sessionId) {
      const cachedSession = this.sessionCache.get(this.amid, toAmid, intent);
      if (cachedSession) {
        sessionId = cachedSession.sessionId;
        this.activeSessions.set(toAmid, sessionId);
        await this.auditLogger.log("SESSION_CACHED", "INFO", `Cache hit for ${toAmid}`);
      } else {
        sessionId = await this.establishSession(toAmid, options);
        const ttlMs = (options.ttl || 3600) * 1e3;
        this.sessionCache.set(sessionId, this.amid, toAmid, intent, ttlMs);
      }
    }
    if (options.unencrypted) {
      await this.transport.send(toAmid, JSON.stringify(message), "message");
    } else {
      const envelope = await this.sessionManager.encryptMessage(sessionId, message);
      const x3dhMsg = this.pendingX3DH.get(toAmid);
      if (x3dhMsg) {
        envelope.x3dh = serializeX3DHMessage(x3dhMsg);
        this.pendingX3DH.delete(toAmid);
      }
      await this.transport.send(toAmid, JSON.stringify(envelope), "message");
    }
    const protocolSession = this.protocolSessions.getSessionsForPeer(toAmid)[0];
    if (protocolSession) {
      this.protocolSessions.recordMessageSent(protocolSession.id);
    }
    await this.auditLogger.logMessageSent(toAmid, sessionId);
  }
  /**
   * Establish a session with an agent via KNOCK protocol.
   *
   * Flow: X3DH key exchange → send KNOCK via relay → activate session
   *
   * Note: Full bidirectional KNOCK (send KNOCK, wait for ACCEPT) requires
   * the receiving agent to process relay "receive" messages and respond.
   * The SDK transport currently doesn't wire relay messages back to the client's
   * KNOCK handler automatically. We send the KNOCK for protocol compliance
   * and activate the session after X3DH succeeds — the shared secret is
   * established by the key exchange, so the initiator can encrypt immediately.
   * The responder will process the KNOCK asynchronously.
   */
  async establishSession(toAmid, options) {
    const registryBundle = await this.registry.getPrekeys(toAmid);
    if (!registryBundle) {
      throw new chunkFNHOFD2H_cjs.SessionError(`Cannot get prekeys for ${toAmid}`, "PREKEY_NOT_FOUND");
    }
    const agentInfo = await this.registry.lookup(toAmid);
    if (!agentInfo) {
      throw new chunkFNHOFD2H_cjs.SessionError(`Cannot find agent ${toAmid}`, "AGENT_NOT_FOUND");
    }
    const bundle = this.convertRegistryBundle(registryBundle);
    const signingKeyB64 = agentInfo.signingPublicKey;
    const signingKey = this.base64Decode(signingKeyB64);
    const { sessionId, x3dhMessage } = await this.sessionManager.initiateSession(
      toAmid,
      bundle,
      signingKey
    );
    this.pendingX3DH.set(toAmid, x3dhMessage);
    this.activeSessions.set(toAmid, sessionId);
    const request = {
      type: options.sessionType || "one-shot",
      ttl: options.ttl || 3600,
      intent: {
        capability: options.intent || "*",
        action: "message"
      },
      priority: options.priority
    };
    try {
      const knock = await this.knockProtocol.createKnock(toAmid, request);
      await this.transport.send(toAmid, JSON.stringify(knock), "knock");
    } catch {
    }
    await this.sessionManager.activateSessionDirect(sessionId);
    this.protocolSessions.createSession(toAmid, request, true);
    await this.auditLogger.logSessionInitiated(toAmid, sessionId);
    return sessionId;
  }
  /**
   * Convert registry prekey bundle to encryption module format.
   */
  convertRegistryBundle(registry) {
    return {
      identityKey: this.base64Decode(registry.identityKey),
      signedPrekey: this.base64Decode(registry.signedPrekey),
      signedPrekeySignature: this.base64Decode(registry.signedPrekeySignature),
      signedPrekeyId: registry.signedPrekeyId,
      oneTimePrekeys: registry.oneTimePrekeys.map((otp) => ({
        id: otp.id,
        key: this.base64Decode(otp.key)
      }))
    };
  }
  /**
   * Register a message handler.
   */
  onMessage(handler) {
    this.messageHandlers.push(handler);
  }
  /**
   * Register an error handler for decryption failures and rejected messages.
   */
  onError(handler) {
    this.errorHandlers.push(handler);
  }
  e2eVerifiedHandler;
  /**
   * Register a handler called when E2E encryption is verified with a peer.
   * Fires once per peer on first successful decrypt (X3DH + Double Ratchet proven).
   */
  onE2EVerified(handler) {
    this.e2eVerifiedHandler = handler;
  }
  emitE2EVerified(peerAmid) {
    if (this.e2eVerifiedPeers.has(peerAmid)) return;
    const isFirst = this.e2eVerifiedPeers.size === 0;
    this.e2eVerifiedPeers.add(peerAmid);
    console.log(`[AGT] E2E VERIFIED with ${peerAmid} (first=${isFirst}, handler=${!!this.e2eVerifiedHandler})`);
    if (this.e2eVerifiedHandler) {
      try {
        this.e2eVerifiedHandler(peerAmid, isFirst);
      } catch (e) {
        console.error("[AGT] e2eVerifiedHandler error:", e?.message || e);
      }
    }
  }
  /**
   * Register a KNOCK handler.
   */
  onKnock(handler) {
    this.knockHandler = handler;
  }
  /**
   * Handle an incoming KNOCK request.
   * This method checks circuit state before policy evaluation.
   */
  async handleIncomingKnock(fromAmid, request) {
    if (this.circuitState === "STOPPED" /* STOPPED */) {
      return { accept: false, reason: "agent_stopped" };
    }
    if (this.circuitState === "PAUSED" /* PAUSED */) {
      return { accept: false, reason: "agent_paused" };
    }
    if (this.isBlocked(fromAmid)) {
      return { accept: false, reason: "blocked" };
    }
    if (this.knockHandler) {
      const result = await this.knockHandler(fromAmid, request);
      if (!result.accept) {
        await this.auditLogger.log(
          "KNOCK_RECEIVED",
          "INFO",
          `KNOCK rejected from ${fromAmid}: ${result.reason || "custom_handler"}`
        );
        return { accept: false, reason: result.reason || "rejected" };
      }
    }
    if (this.policy) {
      const context = {
        fromAmid,
        fromTier: "anonymous",
        // Would need registry lookup for real tier
        fromReputation: 0,
        intentCategory: request.intent.capability,
        requestedTtl: request.ttl
      };
      const policyResult = this.policy.evaluate(context);
      if (!policyResult.allowed) {
        await this.auditLogger.log(
          "KNOCK_RECEIVED",
          "INFO",
          `KNOCK rejected from ${fromAmid}: ${policyResult.reason || "policy"}`
        );
        return { accept: false, reason: policyResult.reason || "policy_rejected" };
      }
    }
    const sessionId = `sess_${Date.now()}_${Math.random().toString(36).slice(2)}`;
    this.protocolSessions.createSession(fromAmid, request, false);
    await this.auditLogger.log(
      "KNOCK_RECEIVED",
      "INFO",
      `KNOCK accepted from ${fromAmid}, session: ${sessionId}`
    );
    this.emitEvent("knock", { fromAmid, request, sessionId });
    this.emitEvent("session_established", { amid: fromAmid, sessionId });
    return { accept: true, sessionId };
  }
  /**
   * Register an event handler.
   */
  on(event, handler) {
    const handlers = this.eventHandlers.get(event) || [];
    handlers.push(handler);
    this.eventHandlers.set(event, handlers);
  }
  /**
   * Remove an event handler.
   */
  off(event, handler) {
    const handlers = this.eventHandlers.get(event) || [];
    const index = handlers.indexOf(handler);
    if (index !== -1) {
      handlers.splice(index, 1);
    }
  }
  /**
   * Set the policy for KNOCK evaluation.
   */
  setPolicy(policy) {
    this.policy = policy;
    this.knockProtocol.setPolicy(policy);
  }
  /**
   * Update registered capabilities.
   */
  async setCapabilities(capabilities) {
    this.capabilities = capabilities;
    if (this.connected) {
      await this.registry.updateCapabilities(this.identity, capabilities);
    }
  }
  /**
   * Get all active sessions.
   */
  getSessions() {
    return this.protocolSessions.getActiveSessions();
  }
  /**
   * Get a specific session.
   */
  getSession(amid) {
    const sessions = this.protocolSessions.getSessionsForPeer(amid);
    return sessions.find((s) => s.state === "ACTIVE" /* ACTIVE */);
  }
  /**
   * Close a session.
   */
  async closeSession(amid) {
    const sessions = this.protocolSessions.getSessionsForPeer(amid);
    for (const session of sessions) {
      this.protocolSessions.closeSession(session.id);
    }
    const sessionId = this.activeSessions.get(amid);
    if (sessionId) {
      this.sessionManager.closeSession(sessionId);
      this.activeSessions.delete(amid);
    }
    this.sessionCache.clearByAmid(amid);
    this.emitEvent("session_closed", { amid });
  }
  // ========== CIRCUIT BREAKERS ==========
  /**
   * Kill a session with a specific peer immediately.
   */
  async killSession(amid) {
    const sessions = this.protocolSessions.getSessionsForPeer(amid);
    for (const session of sessions) {
      this.protocolSessions.closeSession(session.id);
    }
    const sessionId = this.activeSessions.get(amid);
    if (sessionId) {
      this.sessionManager.closeSession(sessionId);
      this.activeSessions.delete(amid);
    }
    if (this.isConnected) {
      try {
        await this.transport.send(amid, JSON.stringify({ type: "close", reason: "session_killed" }), "close");
      } catch {
      }
    }
    this.emitEvent("session_killed", { amid });
    await this.auditLogger.log("SESSION_CLOSED", "INFO", `Session killed for ${amid}`);
  }
  /**
   * Pause accepting new KNOCK requests.
   */
  pauseNew() {
    if (this.circuitState === "STOPPED" /* STOPPED */) {
      throw new chunkFNHOFD2H_cjs.AgentMeshError("Client is stopped", "CLIENT_STOPPED");
    }
    if (this.circuitState !== "PAUSED" /* PAUSED */) {
      this.circuitState = "PAUSED" /* PAUSED */;
      this.circuitStateChangedAt = Date.now();
      this.emitEvent("circuit_paused", { timestamp: this.circuitStateChangedAt });
    }
  }
  /**
   * Resume accepting new KNOCK requests.
   */
  resumeNew() {
    if (this.circuitState === "STOPPED" /* STOPPED */) {
      throw new chunkFNHOFD2H_cjs.AgentMeshError("Client is stopped", "CLIENT_STOPPED");
    }
    if (this.circuitState === "PAUSED" /* PAUSED */) {
      this.circuitState = "RUNNING" /* RUNNING */;
      this.circuitStateChangedAt = Date.now();
      this.emitEvent("circuit_resumed", { timestamp: this.circuitStateChangedAt });
    }
  }
  /**
   * Block a peer and kill their session.
   */
  async block(amid) {
    this.blocklist.add(amid);
    await this.killSession(amid);
    this.sessionCache.clearByAmid(amid);
    this.emitEvent("peer_blocked", { amid });
    await this.auditLogger.log("POLICY_EVALUATED", "WARNING", `Peer blocked: ${amid}`);
  }
  /**
   * Unblock a peer.
   */
  async unblock(amid) {
    this.blocklist.delete(amid);
    this.emitEvent("peer_unblocked", { amid });
  }
  /**
   * Emergency stop - disconnect, reject all, clear sessions.
   * This is a terminal state.
   */
  async emergencyStop() {
    this.circuitState = "STOPPED" /* STOPPED */;
    this.circuitStateChangedAt = Date.now();
    for (const amid of this.activeSessions.keys()) {
      await this.killSession(amid);
    }
    if (this.connected) {
      await this.disconnect();
    }
    this.emitEvent("emergency_stop", { timestamp: this.circuitStateChangedAt });
    await this.auditLogger.log("CONNECTION_LOST", "ERROR", "Emergency stop triggered");
  }
  /**
   * Get current circuit breaker state.
   */
  getCircuitState() {
    return {
      state: this.circuitState,
      changedAt: this.circuitStateChangedAt
    };
  }
  /**
   * Check if a peer is blocked.
   */
  isBlocked(amid) {
    return this.blocklist.has(amid);
  }
  // ========== END CIRCUIT BREAKERS ==========
  // ========== RATE LIMITING ==========
  /**
   * Get rate limit status.
   */
  getRateLimitStatus() {
    if (!this.rateLimiter) {
      return null;
    }
    return this.rateLimiter.getStatus();
  }
  /**
   * Check if rate limit allows sending.
   */
  canSend(peerAmid) {
    if (!this.rateLimiter) {
      return true;
    }
    return this.rateLimiter.canConsume(peerAmid);
  }
  /**
   * Wait for rate limit capacity.
   */
  async waitForSendCapacity(maxWaitMs, peerAmid) {
    if (!this.rateLimiter) {
      return true;
    }
    return this.rateLimiter.waitForCapacity(maxWaitMs, peerAmid);
  }
  // ========== END RATE LIMITING ==========
  // ========== SESSION CACHE ==========
  /**
   * Get session cache statistics.
   */
  getCacheStats() {
    return this.sessionCache.getStats();
  }
  /**
   * Clear cached session for a specific peer and intent.
   */
  clearCachedSession(amid, intent) {
    const cleared = this.sessionCache.clear(this.amid, amid, intent);
    if (cleared) {
      this.emitEvent("cache_cleared", { amid, intent });
    }
    return cleared;
  }
  /**
   * Clear all cached sessions for a peer.
   */
  clearCachedSessionsForPeer(amid) {
    const count = this.sessionCache.clearByAmid(amid);
    if (count > 0) {
      this.emitEvent("cache_cleared", { amid, count });
    }
    return count;
  }
  /**
   * Clear all cached sessions.
   */
  clearAllCachedSessions() {
    this.sessionCache.clearAll();
    this.emitEvent("cache_cleared", { all: true });
  }
  /**
   * Get all cached sessions (for dashboard).
   */
  getCachedSessions() {
    return this.sessionCache.getAll().map((s) => ({
      sessionId: s.sessionId,
      peerAmid: s.receiverAmid === this.amid ? s.initiatorAmid : s.receiverAmid,
      expiresAt: s.expiresAt
    }));
  }
  // ========== END SESSION CACHE ==========
  // ========== OPTIMISTIC SEND ==========
  /**
   * Add a peer to the optimistic send allowlist.
   */
  addOptimisticPeer(amid) {
    this.optimisticAllowlist.add(amid);
  }
  /**
   * Remove a peer from the optimistic send allowlist.
   */
  removeOptimisticPeer(amid) {
    this.optimisticAllowlist.delete(amid);
  }
  /**
   * Check if a peer is in the optimistic send allowlist.
   */
  isOptimisticPeer(amid) {
    return this.optimisticAllowlist.has(amid);
  }
  /**
   * Enable or disable optimistic send.
   */
  setOptimisticSend(enabled) {
    this.optimisticSendEnabled = enabled;
  }
  /**
   * Check if optimistic send should be used for a peer.
   */
  shouldUseOptimisticSend(toAmid, options) {
    if (options.forceOptimistic) {
      return true;
    }
    return this.optimisticSendEnabled && this.optimisticAllowlist.has(toAmid);
  }
  // ========== END OPTIMISTIC SEND ==========
  /**
   * Get client information.
   */
  getInfo() {
    return {
      amid: this.amid,
      connected: this.connected,
      capabilities: this.capabilities,
      activeSessions: this.protocolSessions.getActiveSessions().length,
      registryUrl: this.registryUrl,
      relayUrl: this.relayUrl,
      circuitState: this.circuitState,
      circuitStateChangedAt: this.circuitStateChangedAt
    };
  }
  /**
   * Upload prekeys to registry.
   */
  async uploadPrekeys() {
    const bundle = await this.prekeyManager.loadOrInitialize();
    const serialized = serializePrekeyBundle(bundle);
    await this.registry.uploadPrekeys(
      this.identity,
      serialized.signed_prekey,
      serialized.signed_prekey_signature,
      serialized.signed_prekey_id,
      serialized.one_time_prekeys
    );
    await this.auditLogger.log("PREKEY_ROTATED", "INFO", "Prekeys uploaded");
  }
  /**
   * Rotate prekeys and upload to registry.
   */
  async rotatePrekeys() {
    await this.prekeyManager.loadOrInitialize();
    await this.uploadPrekeys();
  }
  /**
   * Save the client state to storage.
   */
  async save(path = "identity") {
    await this.identity.save(this.storage, path);
  }
  /**
   * Emit an event to handlers.
   */
  emitEvent(event, data) {
    const handlers = this.eventHandlers.get(event) || [];
    for (const handler of handlers) {
      try {
        handler(data);
      } catch {
      }
    }
  }
  /**
   * Base64 decode to bytes.
   */
  base64Decode(b64) {
    let raw = b64;
    if (raw.startsWith("ed25519:")) raw = raw.slice(8);
    else if (raw.startsWith("x25519:")) raw = raw.slice(7);
    const binary = atob(raw);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  }
};

// src/certs/index.ts
function toArrayBuffer3(data) {
  const buffer = new ArrayBuffer(data.length);
  new Uint8Array(buffer).set(data);
  return buffer;
}
function parsePEM(pem) {
  const lines = pem.split("\n");
  const base64Lines = [];
  let inCert = false;
  for (const line of lines) {
    if (line.includes("-----BEGIN CERTIFICATE-----")) {
      inCert = true;
      continue;
    }
    if (line.includes("-----END CERTIFICATE-----")) {
      break;
    }
    if (inCert && line.trim()) {
      base64Lines.push(line.trim());
    }
  }
  const base64 = base64Lines.join("");
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}
function toPEM(der) {
  const binary = String.fromCharCode(...der);
  const base64 = btoa(binary);
  const lines = [];
  for (let i = 0; i < base64.length; i += 64) {
    lines.push(base64.slice(i, i + 64));
  }
  return `-----BEGIN CERTIFICATE-----
${lines.join("\n")}
-----END CERTIFICATE-----`;
}
function parseCertificate(der) {
  if (der[0] !== 48) {
    throw new chunkFNHOFD2H_cjs.ValidationError("Invalid certificate format: expected SEQUENCE");
  }
  const now = /* @__PURE__ */ new Date();
  return {
    raw: der,
    subject: extractSubject(der),
    issuer: extractIssuer(der),
    serialNumber: extractSerialNumber(der),
    notBefore: new Date(now.getTime() - 365 * 24 * 60 * 60 * 1e3),
    // 1 year ago
    notAfter: new Date(now.getTime() + 365 * 24 * 60 * 60 * 1e3),
    // 1 year from now
    publicKey: extractPublicKey(der),
    signatureAlgorithm: "Ed25519",
    signature: extractSignature(der),
    isCA: false,
    keyUsage: ["digitalSignature"],
    extKeyUsage: []
  };
}
function extractSubject(der) {
  return `CN=${bytesToHex(der.slice(4, 12))}`;
}
function extractIssuer(der) {
  return `CN=${bytesToHex(der.slice(12, 20))}`;
}
function extractSerialNumber(der) {
  return bytesToHex(der.slice(0, 8));
}
function extractPublicKey(der) {
  if (der.length >= 64) {
    return der.slice(der.length - 64, der.length - 32);
  }
  return new Uint8Array(32);
}
function extractSignature(der) {
  if (der.length >= 64) {
    return der.slice(der.length - 64);
  }
  return new Uint8Array(64);
}
function bytesToHex(bytes) {
  return Array.from(bytes).map((b) => b.toString(16).padStart(2, "0")).join("");
}
var CertificateManager = class {
  trustedRoots = /* @__PURE__ */ new Map();
  /**
   * Add a trusted root certificate.
   */
  addTrustedRoot(cert) {
    const key = this.getCertificateKey(cert);
    this.trustedRoots.set(key, cert);
  }
  /**
   * Add multiple trusted root certificates.
   */
  addTrustedRoots(certs) {
    for (const cert of certs) {
      this.addTrustedRoot(cert);
    }
  }
  /**
   * Check if a certificate is a trusted root.
   */
  isTrustedRoot(cert) {
    const key = this.getCertificateKey(cert);
    return this.trustedRoots.has(key);
  }
  /**
   * Get a unique key for a certificate.
   */
  getCertificateKey(cert) {
    return `${cert.subject}:${cert.serialNumber}`;
  }
  /**
   * Build a certificate chain from leaf to root.
   */
  buildChain(leafCert, intermediateCerts) {
    const chain = [leafCert];
    let current = leafCert;
    while (!this.isTrustedRoot(current) && current.subject !== current.issuer) {
      const issuer = intermediateCerts.find((c) => c.subject === current.issuer);
      if (!issuer) {
        break;
      }
      chain.push(issuer);
      current = issuer;
    }
    return chain;
  }
  /**
   * Validate a certificate chain.
   */
  async validateChain(chain) {
    if (chain.length === 0) {
      return { valid: false, error: "Empty certificate chain", chain: [] };
    }
    const now = /* @__PURE__ */ new Date();
    for (const cert of chain) {
      if (!this.isValidTime(cert, now)) {
        return {
          valid: false,
          error: `Certificate expired or not yet valid: ${cert.subject}`,
          chain: []
        };
      }
    }
    for (let i = 0; i < chain.length - 1; i++) {
      const current = chain[i];
      const issuer = chain[i + 1];
      if (current.issuer !== issuer.subject) {
        return {
          valid: false,
          error: `Chain broken: ${current.subject} issuer does not match ${issuer.subject}`,
          chain: []
        };
      }
      const signatureValid = await this.verifySignature(current, issuer);
      if (!signatureValid) {
        return {
          valid: false,
          error: `Invalid signature on certificate: ${current.subject}`,
          chain: []
        };
      }
    }
    const lastCert = chain[chain.length - 1];
    if (!this.isTrustedRoot(lastCert) && lastCert.subject !== lastCert.issuer) {
      return {
        valid: false,
        error: "Chain does not end at trusted root",
        chain: []
      };
    }
    if (lastCert.subject === lastCert.issuer) {
      const selfSignatureValid = await this.verifySignature(lastCert, lastCert);
      if (!selfSignatureValid) {
        return {
          valid: false,
          error: "Invalid self-signature on root certificate",
          chain: []
        };
      }
    }
    return { valid: true, chain };
  }
  /**
   * Check if a certificate is valid at a given time.
   */
  isValidTime(cert, time = /* @__PURE__ */ new Date()) {
    return time >= cert.notBefore && time <= cert.notAfter;
  }
  /**
   * Check if a certificate is expired.
   */
  isExpired(cert) {
    return /* @__PURE__ */ new Date() > cert.notAfter;
  }
  /**
   * Verify certificate signature using issuer's public key.
   */
  async verifySignature(cert, issuer) {
    try {
      const publicKey = await crypto.subtle.importKey(
        "raw",
        toArrayBuffer3(issuer.publicKey),
        { name: "Ed25519" },
        false,
        ["verify"]
      );
      const tbsLength = cert.raw.length - cert.signature.length;
      const tbs = cert.raw.slice(0, Math.max(0, tbsLength));
      const result = await crypto.subtle.verify(
        { name: "Ed25519" },
        publicKey,
        toArrayBuffer3(cert.signature),
        toArrayBuffer3(tbs)
      );
      return result;
    } catch {
      return false;
    }
  }
  /**
   * Get all trusted root certificates.
   */
  getTrustedRoots() {
    return Array.from(this.trustedRoots.values());
  }
  /**
   * Clear all trusted roots.
   */
  clearTrustedRoots() {
    this.trustedRoots.clear();
  }
};
function createTrustStore() {
  const roots = [];
  return {
    roots,
    addRoot(cert) {
      roots.push(cert);
    },
    isTrusted(cert) {
      return roots.some(
        (r) => r.subject === cert.subject && r.serialNumber === cert.serialNumber
      );
    }
  };
}

// src/schemas/index.ts
var SchemaValidator = class {
  schemas = /* @__PURE__ */ new Map();
  /**
   * Register a schema.
   */
  register(id, schema) {
    this.schemas.set(id, schema);
  }
  /**
   * Get a registered schema.
   */
  get(id) {
    return this.schemas.get(id);
  }
  /**
   * Validate data against a schema.
   */
  validate(data, schema) {
    const errors = [];
    this.validateValue(data, schema, "", errors);
    return { valid: errors.length === 0, errors };
  }
  /**
   * Validate data against a registered schema by ID.
   */
  validateById(data, schemaId) {
    const schema = this.schemas.get(schemaId);
    if (!schema) {
      return { valid: false, errors: [`Schema not found: ${schemaId}`] };
    }
    return this.validate(data, schema);
  }
  validateValue(data, schema, path, errors) {
    if (schema.$ref) {
      const refSchema = this.schemas.get(schema.$ref);
      if (refSchema) {
        this.validateValue(data, refSchema, path, errors);
      } else {
        errors.push(`${path}: Unknown schema reference: ${schema.$ref}`);
      }
      return;
    }
    if (schema.const !== void 0) {
      if (data !== schema.const) {
        errors.push(`${path}: Expected constant value ${JSON.stringify(schema.const)}`);
      }
      return;
    }
    if (schema.enum) {
      if (!schema.enum.includes(data)) {
        errors.push(`${path}: Value must be one of: ${schema.enum.map((e) => JSON.stringify(e)).join(", ")}`);
      }
      return;
    }
    if (schema.oneOf) {
      const matches = schema.oneOf.filter((s) => {
        const subErrors = [];
        this.validateValue(data, s, path, subErrors);
        return subErrors.length === 0;
      });
      if (matches.length !== 1) {
        errors.push(`${path}: Must match exactly one of the schemas`);
      }
      return;
    }
    if (schema.anyOf) {
      const matches = schema.anyOf.some((s) => {
        const subErrors = [];
        this.validateValue(data, s, path, subErrors);
        return subErrors.length === 0;
      });
      if (!matches) {
        errors.push(`${path}: Must match at least one of the schemas`);
      }
      return;
    }
    if (schema.allOf) {
      for (const s of schema.allOf) {
        this.validateValue(data, s, path, errors);
      }
      return;
    }
    if (schema.type) {
      const types = Array.isArray(schema.type) ? schema.type : [schema.type];
      const actualType = this.getType(data);
      if (!types.includes(actualType)) {
        errors.push(`${path}: Expected type ${types.join(" | ")}, got ${actualType}`);
        return;
      }
      switch (actualType) {
        case "object":
          this.validateObject(data, schema, path, errors);
          break;
        case "array":
          this.validateArray(data, schema, path, errors);
          break;
        case "string":
          this.validateString(data, schema, path, errors);
          break;
        case "number":
        case "integer":
          this.validateNumber(data, schema, path, errors);
          break;
      }
    }
  }
  getType(data) {
    if (data === null) return "null";
    if (Array.isArray(data)) return "array";
    if (typeof data === "number") {
      return Number.isInteger(data) ? "integer" : "number";
    }
    return typeof data;
  }
  validateObject(data, schema, path, errors) {
    if (schema.required) {
      for (const prop of schema.required) {
        if (!(prop in data)) {
          errors.push(`${path}: Missing required property: ${prop}`);
        }
      }
    }
    if (schema.properties) {
      for (const [key, propSchema] of Object.entries(schema.properties)) {
        if (key in data) {
          this.validateValue(data[key], propSchema, `${path}.${key}`, errors);
        }
      }
    }
    if (schema.additionalProperties === false && schema.properties) {
      const allowed = new Set(Object.keys(schema.properties));
      for (const key of Object.keys(data)) {
        if (!allowed.has(key)) {
          errors.push(`${path}: Additional property not allowed: ${key}`);
        }
      }
    }
  }
  validateArray(data, schema, path, errors) {
    if (schema.minItems !== void 0 && data.length < schema.minItems) {
      errors.push(`${path}: Array must have at least ${schema.minItems} items`);
    }
    if (schema.maxItems !== void 0 && data.length > schema.maxItems) {
      errors.push(`${path}: Array must have at most ${schema.maxItems} items`);
    }
    if (schema.items) {
      for (let i = 0; i < data.length; i++) {
        this.validateValue(data[i], schema.items, `${path}[${i}]`, errors);
      }
    }
  }
  validateString(data, schema, path, errors) {
    if (schema.minLength !== void 0 && data.length < schema.minLength) {
      errors.push(`${path}: String must be at least ${schema.minLength} characters`);
    }
    if (schema.maxLength !== void 0 && data.length > schema.maxLength) {
      errors.push(`${path}: String must be at most ${schema.maxLength} characters`);
    }
    if (schema.pattern) {
      const regex = new RegExp(schema.pattern);
      if (!regex.test(data)) {
        errors.push(`${path}: String must match pattern: ${schema.pattern}`);
      }
    }
  }
  validateNumber(data, schema, path, errors) {
    if (schema.minimum !== void 0 && data < schema.minimum) {
      errors.push(`${path}: Number must be >= ${schema.minimum}`);
    }
    if (schema.maximum !== void 0 && data > schema.maximum) {
      errors.push(`${path}: Number must be <= ${schema.maximum}`);
    }
  }
  /**
   * Get all registered schema IDs.
   */
  getRegisteredSchemas() {
    return Array.from(this.schemas.keys());
  }
  /**
   * Clear all registered schemas.
   */
  clear() {
    this.schemas.clear();
  }
};
var CapabilityNegotiator = class {
  capabilities = /* @__PURE__ */ new Map();
  /**
   * Register a capability.
   */
  register(capability) {
    this.capabilities.set(capability.id, capability);
  }
  /**
   * Get a capability by ID.
   */
  get(id) {
    return this.capabilities.get(id);
  }
  /**
   * Match a requested capability against registered capabilities.
   * Supports exact match and wildcard patterns.
   */
  match(requested) {
    const matches = [];
    const hasWildcard = requested.includes("*");
    const requestPattern = hasWildcard ? new RegExp("^" + requested.replace(/\*/g, ".*") + "$") : null;
    for (const [id, cap] of this.capabilities) {
      if (id === requested) {
        matches.push({
          capabilityId: id,
          score: 1,
          exact: true,
          wildcard: false
        });
        continue;
      }
      if (requestPattern && requestPattern.test(id)) {
        matches.push({
          capabilityId: id,
          score: 0.8,
          exact: false,
          wildcard: true
        });
        continue;
      }
      if (requested.startsWith(id + "/")) {
        matches.push({
          capabilityId: id,
          score: 0.5,
          exact: false,
          wildcard: false
        });
      }
    }
    matches.sort((a, b) => b.score - a.score);
    return matches;
  }
  /**
   * Check if a capability supports an action.
   */
  supportsAction(capabilityId, action) {
    const capability = this.capabilities.get(capabilityId);
    if (!capability) return false;
    return capability.actions.includes(action) || capability.actions.includes("*");
  }
  /**
   * Get all registered capabilities.
   */
  getAll() {
    return Array.from(this.capabilities.values());
  }
  /**
   * Clear all registered capabilities.
   */
  clear() {
    this.capabilities.clear();
  }
};
var SequenceTracker = class {
  sequences = /* @__PURE__ */ new Map();
  /**
   * Start a new sequence.
   */
  start(id, capabilityId, steps) {
    const sequence = {
      id,
      capabilityId,
      steps,
      currentStep: 0,
      complete: false
    };
    this.sequences.set(id, sequence);
    return sequence;
  }
  /**
   * Get a sequence by ID.
   */
  get(id) {
    return this.sequences.get(id);
  }
  /**
   * Advance the sequence to the next step.
   */
  advance(id) {
    const sequence = this.sequences.get(id);
    if (!sequence) {
      return { success: false, error: "Sequence not found" };
    }
    if (sequence.complete) {
      return { success: false, error: "Sequence already complete" };
    }
    sequence.currentStep++;
    if (sequence.currentStep >= sequence.steps.length) {
      sequence.complete = true;
      return { success: true, complete: true };
    }
    return { success: true, complete: false };
  }
  /**
   * Get the current step of a sequence.
   */
  getCurrentStep(id) {
    const sequence = this.sequences.get(id);
    if (!sequence || sequence.complete) return void 0;
    return sequence.steps[sequence.currentStep];
  }
  /**
   * Validate that a message matches the expected step.
   */
  validateStep(id, direction, data, validator) {
    const step = this.getCurrentStep(id);
    if (!step) {
      return { valid: false, error: "No current step or sequence not found" };
    }
    if (step.direction !== direction) {
      if (step.optional) {
        this.advance(id);
        return this.validateStep(id, direction, data, validator);
      }
      return {
        valid: false,
        error: `Expected ${step.direction} message, got ${direction}`
      };
    }
    if (step.schema && validator) {
      const result = validator.validate(data, step.schema);
      if (!result.valid) {
        return {
          valid: false,
          error: `Schema validation failed: ${result.errors.join(", ")}`
        };
      }
    }
    return { valid: true };
  }
  /**
   * Complete the current sequence.
   */
  complete(id) {
    const sequence = this.sequences.get(id);
    if (sequence) {
      sequence.complete = true;
    }
  }
  /**
   * Remove a sequence.
   */
  remove(id) {
    this.sequences.delete(id);
  }
  /**
   * Get all active sequences.
   */
  getActive() {
    return Array.from(this.sequences.values()).filter((s) => !s.complete);
  }
  /**
   * Clear all sequences.
   */
  clear() {
    this.sequences.clear();
  }
};
var BUILTIN_SCHEMAS = {
  "agentmesh/knock": {
    type: "object",
    required: ["version", "from", "to", "request", "timestamp", "nonce", "signature"],
    properties: {
      version: { type: "string", pattern: "^agentmesh/\\d+\\.\\d+$" },
      from: { type: "string", minLength: 20 },
      to: { type: "string", minLength: 20 },
      request: {
        type: "object",
        required: ["type", "ttl", "intent"],
        properties: {
          type: { enum: ["one-shot", "streaming", "persistent"] },
          ttl: { type: "integer", minimum: 1 },
          expectedMessages: { type: "integer", minimum: 1 },
          intent: {
            type: "object",
            required: ["capability", "action"],
            properties: {
              capability: { type: "string", minLength: 1 },
              action: { type: "string", minLength: 1 },
              params: { type: "object" }
            }
          }
        }
      },
      timestamp: { type: "integer" },
      nonce: { type: "string", minLength: 16 },
      signature: { type: "string", minLength: 1 },
      certificateChain: { type: "array", items: { type: "string" } }
    }
  },
  "agentmesh/knock-response": {
    type: "object",
    required: ["type", "timestamp", "from", "to", "knockNonce", "signature"],
    properties: {
      type: { enum: ["ACCEPT", "REJECT"] },
      sessionId: { type: "string" },
      reason: { type: "string" },
      timestamp: { type: "integer" },
      from: { type: "string", minLength: 20 },
      to: { type: "string", minLength: 20 },
      knockNonce: { type: "string", minLength: 16 },
      signature: { type: "string", minLength: 1 }
    }
  },
  "agentmesh/message": {
    type: "object",
    required: ["from", "to", "sessionId", "sequence", "timestamp", "payload"],
    properties: {
      from: { type: "string", minLength: 20 },
      to: { type: "string", minLength: 20 },
      sessionId: { type: "string", minLength: 1 },
      sequence: { type: "integer", minimum: 0 },
      timestamp: { type: "integer" },
      payload: { type: "object" },
      encrypted: { type: "boolean" }
    }
  }
};
function createValidator() {
  const validator = new SchemaValidator();
  for (const [id, schema] of Object.entries(BUILTIN_SCHEMAS)) {
    validator.register(id, schema);
  }
  return validator;
}

// src/did/index.ts
function toMultibase(bytes) {
  const ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let num = 0n;
  for (const byte of bytes) {
    num = num * 256n + BigInt(byte);
  }
  let base58 = "";
  while (num > 0n) {
    const remainder = Number(num % 58n);
    num = num / 58n;
    base58 = ALPHABET[remainder] + base58;
  }
  for (const byte of bytes) {
    if (byte === 0) {
      base58 = "1" + base58;
    } else {
      break;
    }
  }
  return "z" + base58;
}
function toArrayBuffer4(data) {
  const buffer = new ArrayBuffer(data.length);
  new Uint8Array(buffer).set(data);
  return buffer;
}
function fromMultibase(multibase) {
  const ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  if (!multibase.startsWith("z")) {
    throw new chunkFNHOFD2H_cjs.ValidationError("Unsupported multibase format");
  }
  const base58 = multibase.slice(1);
  let num = 0n;
  for (const char of base58) {
    const index = ALPHABET.indexOf(char);
    if (index === -1) {
      throw new chunkFNHOFD2H_cjs.ValidationError("Invalid base58 character");
    }
    num = num * 58n + BigInt(index);
  }
  const bytes = [];
  while (num > 0n) {
    bytes.unshift(Number(num % 256n));
    num = num / 256n;
  }
  for (const char of base58) {
    if (char === "1") {
      bytes.unshift(0);
    } else {
      break;
    }
  }
  return new Uint8Array(bytes);
}
var DIDManager = class {
  static CONTEXT = [
    "https://www.w3.org/ns/did/v1",
    "https://w3id.org/security/suites/ed25519-2020/v1",
    "https://w3id.org/security/suites/x25519-2020/v1"
  ];
  /**
   * Create a DID from an AMID.
   */
  static createDID(amid) {
    return `did:agentmesh:${amid}`;
  }
  /**
   * Extract AMID from a DID.
   */
  static extractAmid(did) {
    const match = did.match(/^did:agentmesh:(.+)$/);
    return match ? match[1] : null;
  }
  /**
   * Create a DID Document from an identity.
   */
  static createDocument(identity, options) {
    const did = this.createDID(identity.amid);
    const signingVerificationMethod = {
      id: `${did}#signing-key`,
      type: "Ed25519VerificationKey2020",
      controller: did,
      publicKeyMultibase: toMultibase(identity.getSigningPublicKeyRaw())
    };
    const keyAgreementMethod = {
      id: `${did}#key-agreement`,
      type: "X25519KeyAgreementKey2020",
      controller: did,
      publicKeyMultibase: toMultibase(identity.getExchangePublicKeyRaw())
    };
    const document = {
      "@context": this.CONTEXT,
      id: did,
      verificationMethod: [signingVerificationMethod, keyAgreementMethod],
      authentication: [`${did}#signing-key`],
      assertionMethod: [`${did}#signing-key`],
      keyAgreement: [`${did}#key-agreement`],
      metadata: {
        created: (/* @__PURE__ */ new Date()).toISOString()
      }
    };
    if (options?.alsoKnownAs) {
      document.alsoKnownAs = options.alsoKnownAs;
    }
    if (options?.controller) {
      document.controller = options.controller;
    }
    if (options?.serviceEndpoints) {
      document.service = options.serviceEndpoints;
    }
    return document;
  }
  /**
   * Sign a DID document.
   */
  static async signDocument(document, identity) {
    const documentJson = JSON.stringify(document, Object.keys(document).sort());
    const documentBytes = new TextEncoder().encode(documentJson);
    const signature = await identity.sign(documentBytes);
    return {
      document,
      proof: {
        type: "Ed25519Signature2020",
        created: (/* @__PURE__ */ new Date()).toISOString(),
        verificationMethod: `${document.id}#signing-key`,
        proofPurpose: "assertionMethod",
        proofValue: toMultibase(signature)
      }
    };
  }
  /**
   * Verify a signed DID document.
   */
  static async verifyDocument(signedDocument, publicKey) {
    const { document, proof } = signedDocument;
    let verifyKey = publicKey;
    if (!verifyKey) {
      const verificationMethod = document.verificationMethod?.find(
        (vm) => vm.id === proof.verificationMethod
      );
      if (!verificationMethod?.publicKeyMultibase) {
        return { valid: false, error: "Cannot find verification method" };
      }
      verifyKey = fromMultibase(verificationMethod.publicKeyMultibase);
    }
    const documentJson = JSON.stringify(document, Object.keys(document).sort());
    const documentBytes = new TextEncoder().encode(documentJson);
    const signature = fromMultibase(proof.proofValue);
    try {
      const key = await crypto.subtle.importKey(
        "raw",
        toArrayBuffer4(verifyKey),
        { name: "Ed25519" },
        false,
        ["verify"]
      );
      const valid = await crypto.subtle.verify(
        { name: "Ed25519" },
        key,
        toArrayBuffer4(signature),
        toArrayBuffer4(documentBytes)
      );
      return { valid };
    } catch (error) {
      return { valid: false, error: String(error) };
    }
  }
  /**
   * Add a service endpoint to a document.
   */
  static addService(document, service) {
    const updated = { ...document };
    updated.service = [...updated.service || [], service];
    if (updated.metadata) {
      updated.metadata.updated = (/* @__PURE__ */ new Date()).toISOString();
    }
    return updated;
  }
  /**
   * Remove a service endpoint from a document.
   */
  static removeService(document, serviceId) {
    const updated = { ...document };
    updated.service = (updated.service || []).filter((s) => s.id !== serviceId);
    if (updated.metadata) {
      updated.metadata.updated = (/* @__PURE__ */ new Date()).toISOString();
    }
    return updated;
  }
  /**
   * Deactivate a DID document.
   */
  static deactivate(document) {
    const updated = { ...document };
    updated.metadata = {
      ...updated.metadata,
      deactivated: true,
      updated: (/* @__PURE__ */ new Date()).toISOString()
    };
    return updated;
  }
  /**
   * Serialize a DID document to JSON-LD.
   */
  static serialize(document) {
    return JSON.stringify(document, null, 2);
  }
  /**
   * Deserialize a DID document from JSON.
   */
  static deserialize(json) {
    const doc = JSON.parse(json);
    if (!doc.id || !doc["@context"]) {
      throw new chunkFNHOFD2H_cjs.ValidationError("Invalid DID document: missing required fields");
    }
    if (!doc.id.startsWith("did:")) {
      throw new chunkFNHOFD2H_cjs.ValidationError("Invalid DID format");
    }
    return doc;
  }
};
var DIDResolver = class {
  cache = /* @__PURE__ */ new Map();
  cacheTtlMs;
  constructor(options) {
    this.cacheTtlMs = options?.cacheTtlMs || 5 * 60 * 1e3;
  }
  /**
   * Resolve a DID to its document.
   * This is a local resolver - for AgentMesh DIDs, we need the registry.
   */
  async resolve(did) {
    const cached = this.cache.get(did);
    if (cached && Date.now() - cached.timestamp < this.cacheTtlMs) {
      return {
        didDocument: cached.document,
        didResolutionMetadata: { contentType: "application/did+ld+json" },
        didDocumentMetadata: cached.document.metadata || {}
      };
    }
    const match = did.match(/^did:([^:]+):(.+)$/);
    if (!match) {
      return {
        didDocument: null,
        didResolutionMetadata: {
          error: "invalidDid",
          errorMessage: "Invalid DID format"
        },
        didDocumentMetadata: {}
      };
    }
    const [, method, identifier] = match;
    if (method === "agentmesh") {
      return {
        didDocument: null,
        didResolutionMetadata: {
          error: "notFound",
          errorMessage: "DID document not found. Use registry lookup."
        },
        didDocumentMetadata: {}
      };
    }
    return {
      didDocument: null,
      didResolutionMetadata: {
        error: "methodNotSupported",
        errorMessage: `DID method '${method}' is not supported`
      },
      didDocumentMetadata: {}
    };
  }
  /**
   * Cache a resolved document.
   */
  cacheDocument(did, document) {
    this.cache.set(did, { document, timestamp: Date.now() });
  }
  /**
   * Invalidate a cached document.
   */
  invalidate(did) {
    this.cache.delete(did);
  }
  /**
   * Clear the entire cache.
   */
  clearCache() {
    this.cache.clear();
  }
  /**
   * Get cache statistics.
   */
  getCacheStats() {
    return {
      size: this.cache.size,
      hits: 0,
      // Would need to track in resolve()
      misses: 0
    };
  }
};
function createRelayServiceEndpoint(did, relayUrl) {
  return {
    id: `${did}#relay`,
    type: "AgentMeshRelay",
    serviceEndpoint: relayUrl
  };
}
function createDHTServiceEndpoint(did, dhtNode) {
  return {
    id: `${did}#dht`,
    type: "AgentMeshDHT",
    serviceEndpoint: dhtNode
  };
}

// src/dht/index.ts
var KBucket = class {
  k;
  nodes = [];
  constructor(k = 20) {
    this.k = k;
  }
  /**
   * Get all nodes in this bucket.
   */
  getNodes() {
    return [...this.nodes];
  }
  /**
   * Get the number of nodes in this bucket.
   */
  get size() {
    return this.nodes.length;
  }
  /**
   * Check if the bucket is full.
   */
  get isFull() {
    return this.nodes.length >= this.k;
  }
  /**
   * Add or update a node in the bucket.
   */
  addOrUpdate(node) {
    const existingIndex = this.nodes.findIndex((n) => n.id === node.id);
    if (existingIndex !== -1) {
      this.nodes.splice(existingIndex, 1);
      this.nodes.push({ ...node, lastSeen: /* @__PURE__ */ new Date() });
      return true;
    }
    if (!this.isFull) {
      this.nodes.push(node);
      return true;
    }
    return false;
  }
  /**
   * Remove a node from the bucket.
   */
  remove(nodeId) {
    const index = this.nodes.findIndex((n) => n.id === nodeId);
    if (index !== -1) {
      this.nodes.splice(index, 1);
      return true;
    }
    return false;
  }
  /**
   * Get the oldest node (for potential eviction).
   */
  getOldest() {
    return this.nodes[0];
  }
  /**
   * Clear all nodes from the bucket.
   */
  clear() {
    this.nodes = [];
  }
};
function xorDistance(id1, id2) {
  const bytes1 = decodeId(id1);
  const bytes2 = decodeId(id2);
  const length = Math.max(bytes1.length, bytes2.length);
  const result = new Uint8Array(length);
  for (let i = 0; i < length; i++) {
    const b1 = i < bytes1.length ? bytes1[i] : 0;
    const b2 = i < bytes2.length ? bytes2[i] : 0;
    result[i] = b1 ^ b2;
  }
  return result;
}
function compareDistance(a, b) {
  const length = Math.max(a.length, b.length);
  for (let i = 0; i < length; i++) {
    const ai = i < a.length ? a[i] : 0;
    const bi = i < b.length ? b[i] : 0;
    if (ai < bi) return -1;
    if (ai > bi) return 1;
  }
  return 0;
}
function getBucketIndex(ourId, nodeId) {
  const distance = xorDistance(ourId, nodeId);
  for (let i = 0; i < distance.length; i++) {
    const byte = distance[i];
    if (byte !== 0) {
      for (let bit = 7; bit >= 0; bit--) {
        if ((byte & 1 << bit) !== 0) {
          return i * 8 + (7 - bit);
        }
      }
    }
  }
  return 0;
}
function decodeId(id) {
  const ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let num = 0n;
  for (const char of id) {
    const index = ALPHABET.indexOf(char);
    if (index === -1) {
      const bytes2 = new Uint8Array(id.length / 2);
      for (let i = 0; i < id.length; i += 2) {
        bytes2[i / 2] = parseInt(id.slice(i, i + 2), 16);
      }
      return bytes2;
    }
    num = num * 58n + BigInt(index);
  }
  const bytes = [];
  while (num > 0n) {
    bytes.unshift(Number(num % 256n));
    num = num / 256n;
  }
  for (const char of id) {
    if (char === "1") {
      bytes.unshift(0);
    } else {
      break;
    }
  }
  return new Uint8Array(bytes);
}
var DHTClient = class {
  identity;
  bootstrapNodes;
  buckets = /* @__PURE__ */ new Map();
  storage = /* @__PURE__ */ new Map();
  metrics;
  k;
  alpha;
  connected = false;
  constructor(identity, options) {
    this.identity = identity;
    this.bootstrapNodes = options?.bootstrapNodes || [];
    this.k = options?.k || 20;
    this.alpha = options?.alpha || 3;
    this.metrics = {
      knownNodes: 0,
      storedEntries: 0,
      lookups: 0,
      avgLookupTimeMs: 0,
      failedLookups: 0
    };
  }
  /**
   * Connect to the DHT network.
   */
  async connect() {
    if (this.connected) return;
    for (const nodeUrl of this.bootstrapNodes) {
      try {
        await this.pingNode(nodeUrl);
      } catch {
      }
    }
    this.connected = true;
  }
  /**
   * Disconnect from the DHT network.
   */
  async disconnect() {
    this.connected = false;
    this.buckets.clear();
  }
  /**
   * Check if connected to the DHT.
   */
  get isAvailable() {
    return this.connected && this.getKnownNodesCount() > 0;
  }
  /**
   * Ping a node to check if it's alive.
   */
  async pingNode(nodeUrl) {
    const node = {
      id: this.hashUrl(nodeUrl),
      address: nodeUrl,
      lastSeen: /* @__PURE__ */ new Date()
    };
    this.addNode(node);
    return node;
  }
  /**
   * Add a node to the routing table.
   */
  addNode(node) {
    if (node.id === this.identity.amid) return false;
    const bucketIndex = getBucketIndex(this.identity.amid, node.id);
    let bucket = this.buckets.get(bucketIndex);
    if (!bucket) {
      bucket = new KBucket(this.k);
      this.buckets.set(bucketIndex, bucket);
    }
    const added = bucket.addOrUpdate(node);
    if (added) {
      this.updateMetrics();
    }
    return added;
  }
  /**
   * Store a value in the DHT.
   */
  async put(key, value, ttlSeconds = 3600) {
    const signature = await this.identity.sign(value);
    const entry = {
      key,
      value,
      publisher: this.identity.amid,
      signature,
      expiresAt: new Date(Date.now() + ttlSeconds * 1e3),
      updatedAt: /* @__PURE__ */ new Date()
    };
    this.storage.set(key, entry);
    this.metrics.storedEntries = this.storage.size;
  }
  /**
   * Retrieve a value from the DHT.
   */
  async get(key) {
    const startTime = Date.now();
    this.metrics.lookups++;
    const local = this.storage.get(key);
    if (local) {
      if (local.expiresAt > /* @__PURE__ */ new Date()) {
        this.updateLookupTime(startTime);
        return local;
      }
      this.storage.delete(key);
    }
    this.metrics.failedLookups++;
    return null;
  }
  /**
   * Register an agent with capabilities.
   */
  async registerAgent(capabilities) {
    const timestamp = Date.now();
    const data = JSON.stringify({
      amid: this.identity.amid,
      capabilities,
      timestamp
    });
    const signature = await this.identity.sign(new TextEncoder().encode(data));
    const entry = {
      amid: this.identity.amid,
      capabilities,
      address: "",
      // Would be set by caller
      timestamp,
      signature: btoa(String.fromCharCode(...signature))
    };
    for (const capability of capabilities) {
      const key = `capability:${capability}`;
      const value = new TextEncoder().encode(JSON.stringify(entry));
      await this.put(key, value, 3600);
    }
  }
  /**
   * Find agents with a specific capability.
   */
  async findAgents(capability) {
    const key = `capability:${capability}`;
    const entry = await this.get(key);
    if (!entry) {
      return [];
    }
    try {
      const data = JSON.parse(new TextDecoder().decode(entry.value));
      return [data];
    } catch {
      return [];
    }
  }
  /**
   * Perform iterative lookup for a key.
   */
  async iterativeLookup(targetId) {
    const closest = this.getClosestNodes(targetId, this.alpha);
    return closest;
  }
  /**
   * Get the k closest nodes to a target.
   */
  getClosestNodes(targetId, count = this.k) {
    const allNodes = [];
    for (const bucket of this.buckets.values()) {
      allNodes.push(...bucket.getNodes());
    }
    allNodes.sort((a, b) => {
      const distA = xorDistance(targetId, a.id);
      const distB = xorDistance(targetId, b.id);
      return compareDistance(distA, distB);
    });
    return allNodes.slice(0, count);
  }
  /**
   * Get the number of known nodes.
   */
  getKnownNodesCount() {
    let count = 0;
    for (const bucket of this.buckets.values()) {
      count += bucket.size;
    }
    return count;
  }
  /**
   * Get DHT metrics.
   */
  getMetrics() {
    return { ...this.metrics };
  }
  /**
   * Clear all stored data and nodes.
   */
  clear() {
    this.buckets.clear();
    this.storage.clear();
    this.metrics = {
      knownNodes: 0,
      storedEntries: 0,
      lookups: 0,
      avgLookupTimeMs: 0,
      failedLookups: 0
    };
  }
  /**
   * Hash a URL to a node ID (for testing).
   */
  hashUrl(url) {
    let hash = 0;
    for (const char of url) {
      hash = (hash << 5) - hash + char.charCodeAt(0);
      hash = hash & hash;
    }
    return Math.abs(hash).toString(16).padStart(16, "0");
  }
  /**
   * Update metrics.
   */
  updateMetrics() {
    this.metrics.knownNodes = this.getKnownNodesCount();
    this.metrics.storedEntries = this.storage.size;
  }
  /**
   * Update average lookup time.
   */
  updateLookupTime(startTime) {
    const lookupTime = Date.now() - startTime;
    const totalLookups = this.metrics.lookups;
    this.metrics.avgLookupTimeMs = (this.metrics.avgLookupTimeMs * (totalLookups - 1) + lookupTime) / totalLookups;
  }
};
function createCapabilityKey(capability) {
  return `cap:${capability}`;
}
function createAmidKey(amid) {
  return `agent:${amid}`;
}
var DashboardError = class extends Error {
  code;
  constructor(message, code) {
    super(message);
    this.name = "DashboardError";
    this.code = code;
    Object.setPrototypeOf(this, new.target.prototype);
  }
};
var Dashboard = class {
  client;
  port;
  apiKey;
  corsEnabled;
  server = null;
  eventHandlers = [];
  running = false;
  constructor(client, config = {}) {
    this.client = client;
    this.port = config.port ?? 3847;
    this.apiKey = config.apiKey;
    this.corsEnabled = config.cors ?? true;
  }
  /**
   * Check if dashboard is running.
   */
  get isRunning() {
    return this.running;
  }
  /**
   * Start the dashboard server.
   */
  async start() {
    if (this.running) {
      throw new DashboardError("Dashboard already running", "ALREADY_RUNNING");
    }
    return new Promise((resolve, reject) => {
      this.server = http.createServer((req, res) => this.handleRequest(req, res));
      this.server.on("error", (err) => {
        if (err.code === "EADDRINUSE") {
          reject(new DashboardError(`Port ${this.port} is already in use`, "PORT_IN_USE"));
        } else {
          reject(new DashboardError(err.message, "SERVER_ERROR"));
        }
      });
      this.server.listen(this.port, "127.0.0.1", () => {
        this.running = true;
        this.emitEvent("dashboard_started", { port: this.port });
        resolve();
      });
    });
  }
  /**
   * Stop the dashboard server.
   */
  async stop() {
    if (!this.running || !this.server) {
      return;
    }
    return new Promise((resolve) => {
      this.server.close(() => {
        this.running = false;
        this.server = null;
        this.emitEvent("dashboard_stopped", {});
        resolve();
      });
    });
  }
  /**
   * Register an event handler.
   */
  onEvent(handler) {
    this.eventHandlers.push(handler);
  }
  /**
   * Handle incoming HTTP request.
   */
  async handleRequest(req, res) {
    if (this.corsEnabled) {
      const origin = req.headers.origin;
      if (origin && (origin.startsWith("http://localhost") || origin.startsWith("http://127.0.0.1"))) {
        res.setHeader("Access-Control-Allow-Origin", origin);
        res.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
        res.setHeader("Access-Control-Allow-Headers", "Content-Type, X-API-Key");
      }
    }
    if (req.method === "OPTIONS") {
      res.writeHead(204);
      res.end();
      return;
    }
    if (this.apiKey) {
      const providedKey = req.headers["x-api-key"];
      if (providedKey !== this.apiKey) {
        this.sendJSON(res, 401, { error: "Unauthorized", code: "UNAUTHORIZED" });
        return;
      }
    }
    const url = req.url || "/";
    const method = req.method || "GET";
    try {
      if (url === "/status" && method === "GET") {
        await this.handleGetStatus(res);
      } else if (url === "/sessions" && method === "GET") {
        await this.handleGetSessions(res);
      } else if (url.startsWith("/sessions/") && url.endsWith("/kill") && method === "POST") {
        const amid = url.slice("/sessions/".length, -"/kill".length);
        await this.handleKillSession(res, amid);
      } else if (url === "/policy" && method === "GET") {
        await this.handleGetPolicy(res);
      } else if (url === "/policy" && method === "POST") {
        const body = await this.readBody(req);
        await this.handleSetPolicy(res, body);
      } else if (url === "/circuit/pause" && method === "POST") {
        await this.handleCircuitPause(res);
      } else if (url === "/circuit/resume" && method === "POST") {
        await this.handleCircuitResume(res);
      } else if (url === "/circuit/emergency-stop" && method === "POST") {
        await this.handleEmergencyStop(res);
      } else {
        this.sendJSON(res, 404, { error: "Not found", code: "NOT_FOUND" });
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : "Internal server error";
      this.sendJSON(res, 500, { error: message, code: "INTERNAL_ERROR" });
      this.emitEvent("dashboard_error", { error });
    }
  }
  /**
   * GET /status - Get client status.
   */
  async handleGetStatus(res) {
    const info = this.client.getInfo();
    const rateLimitStatus = this.client.getRateLimitStatus();
    this.sendJSON(res, 200, {
      amid: info.amid,
      connected: info.connected,
      capabilities: info.capabilities,
      activeSessions: info.activeSessions,
      circuitState: info.circuitState,
      circuitStateChangedAt: info.circuitStateChangedAt,
      rateLimit: rateLimitStatus
    });
  }
  /**
   * GET /sessions - Get active sessions.
   */
  async handleGetSessions(res) {
    const sessions = this.client.getSessions();
    this.sendJSON(res, 200, {
      count: sessions.length,
      sessions: sessions.map((s) => ({
        id: s.id,
        remoteAmid: s.remoteAmid,
        state: s.state,
        isInitiator: s.isInitiator,
        createdAt: s.createdAt.toISOString(),
        lastActivity: s.lastActivity?.toISOString(),
        messagesSent: s.messagesSent,
        messagesReceived: s.messagesReceived
      }))
    });
  }
  /**
   * POST /sessions/:amid/kill - Kill a session.
   */
  async handleKillSession(res, amid) {
    if (!amid) {
      this.sendJSON(res, 400, { error: "AMID required", code: "INVALID_REQUEST" });
      return;
    }
    await this.client.killSession(amid);
    this.sendJSON(res, 200, { success: true, amid });
  }
  /**
   * GET /policy - Get current policy.
   */
  async handleGetPolicy(res) {
    const info = this.client.getInfo();
    this.sendJSON(res, 200, {
      capabilities: info.capabilities,
      circuitState: info.circuitState
    });
  }
  /**
   * POST /policy - Update policy.
   */
  async handleSetPolicy(res, body) {
    try {
      const data = JSON.parse(body);
      if (data.capabilities && Array.isArray(data.capabilities)) {
        await this.client.setCapabilities(data.capabilities);
      }
      if (data.policy) {
        const policy = new chunkUBUGIENK_cjs.Policy(data.policy);
        this.client.setPolicy(policy);
      }
      this.sendJSON(res, 200, { success: true });
    } catch (error) {
      const message = error instanceof Error ? error.message : "Invalid request body";
      this.sendJSON(res, 400, { error: message, code: "INVALID_REQUEST" });
    }
  }
  /**
   * POST /circuit/pause - Pause accepting new sessions.
   */
  async handleCircuitPause(res) {
    try {
      this.client.pauseNew();
      this.sendJSON(res, 200, { success: true, state: "PAUSED" /* PAUSED */ });
    } catch (error) {
      const message = error instanceof Error ? error.message : "Failed to pause";
      this.sendJSON(res, 400, { error: message, code: "PAUSE_FAILED" });
    }
  }
  /**
   * POST /circuit/resume - Resume accepting new sessions.
   */
  async handleCircuitResume(res) {
    try {
      this.client.resumeNew();
      this.sendJSON(res, 200, { success: true, state: "RUNNING" /* RUNNING */ });
    } catch (error) {
      const message = error instanceof Error ? error.message : "Failed to resume";
      this.sendJSON(res, 400, { error: message, code: "RESUME_FAILED" });
    }
  }
  /**
   * POST /circuit/emergency-stop - Emergency stop (terminal).
   */
  async handleEmergencyStop(res) {
    await this.client.emergencyStop();
    this.sendJSON(res, 200, { success: true, state: "STOPPED" /* STOPPED */ });
  }
  /**
   * Read request body.
   */
  async readBody(req) {
    return new Promise((resolve, reject) => {
      let body = "";
      req.on("data", (chunk) => {
        body += chunk.toString();
        if (body.length > 1024 * 1024) {
          reject(new Error("Request body too large"));
        }
      });
      req.on("end", () => resolve(body));
      req.on("error", reject);
    });
  }
  /**
   * Send JSON response.
   */
  sendJSON(res, status, data) {
    res.writeHead(status, { "Content-Type": "application/json" });
    res.end(JSON.stringify(data));
  }
  /**
   * Emit an event.
   */
  emitEvent(type, data) {
    for (const handler of this.eventHandlers) {
      try {
        handler({ type, data });
      } catch {
      }
    }
  }
};

// src/index.ts
var VERSION = "0.1.0";
var PROTOCOL_VERSION = "agentmesh/0.2";

Object.defineProperty(exports, "Identity", {
  enumerable: true,
  get: function () { return chunkBPYP43TA_cjs.Identity; }
});
Object.defineProperty(exports, "Config", {
  enumerable: true,
  get: function () { return chunkUBUGIENK_cjs.Config; }
});
Object.defineProperty(exports, "ConfigError", {
  enumerable: true,
  get: function () { return chunkUBUGIENK_cjs.ConfigError; }
});
Object.defineProperty(exports, "FileConfigLoader", {
  enumerable: true,
  get: function () { return chunkUBUGIENK_cjs.FileConfigLoader; }
});
Object.defineProperty(exports, "Policy", {
  enumerable: true,
  get: function () { return chunkUBUGIENK_cjs.Policy; }
});
Object.defineProperty(exports, "RegistryClient", {
  enumerable: true,
  get: function () { return chunkUBUGIENK_cjs.RegistryClient; }
});
Object.defineProperty(exports, "Tier", {
  enumerable: true,
  get: function () { return chunkUBUGIENK_cjs.Tier; }
});
Object.defineProperty(exports, "TierLevel", {
  enumerable: true,
  get: function () { return chunkUBUGIENK_cjs.TierLevel; }
});
Object.defineProperty(exports, "createFileConfigLoader", {
  enumerable: true,
  get: function () { return chunkUBUGIENK_cjs.createFileConfigLoader; }
});
Object.defineProperty(exports, "getTierLevel", {
  enumerable: true,
  get: function () { return chunkUBUGIENK_cjs.getTierLevel; }
});
Object.defineProperty(exports, "P2PTransport", {
  enumerable: true,
  get: function () { return chunkFAEZQCEA_cjs.P2PTransport; }
});
Object.defineProperty(exports, "RelayTransport", {
  enumerable: true,
  get: function () { return chunkFAEZQCEA_cjs.RelayTransport; }
});
Object.defineProperty(exports, "createP2PTransport", {
  enumerable: true,
  get: function () { return chunkFAEZQCEA_cjs.createP2PTransport; }
});
Object.defineProperty(exports, "FileStorage", {
  enumerable: true,
  get: function () { return chunkC7KJHFTP_cjs.FileStorage; }
});
Object.defineProperty(exports, "KVStorage", {
  enumerable: true,
  get: function () { return chunkC7KJHFTP_cjs.KVStorage; }
});
Object.defineProperty(exports, "MemoryStorage", {
  enumerable: true,
  get: function () { return chunkC7KJHFTP_cjs.MemoryStorage; }
});
Object.defineProperty(exports, "R2Storage", {
  enumerable: true,
  get: function () { return chunkC7KJHFTP_cjs.R2Storage; }
});
Object.defineProperty(exports, "AgentMeshError", {
  enumerable: true,
  get: function () { return chunkFNHOFD2H_cjs.AgentMeshError; }
});
Object.defineProperty(exports, "CryptoError", {
  enumerable: true,
  get: function () { return chunkFNHOFD2H_cjs.CryptoError; }
});
Object.defineProperty(exports, "NetworkError", {
  enumerable: true,
  get: function () { return chunkFNHOFD2H_cjs.NetworkError; }
});
Object.defineProperty(exports, "SessionError", {
  enumerable: true,
  get: function () { return chunkFNHOFD2H_cjs.SessionError; }
});
Object.defineProperty(exports, "StorageError", {
  enumerable: true,
  get: function () { return chunkFNHOFD2H_cjs.StorageError; }
});
Object.defineProperty(exports, "ValidationError", {
  enumerable: true,
  get: function () { return chunkFNHOFD2H_cjs.ValidationError; }
});
exports.AgentMeshClient = AgentMeshClient;
exports.AuditLogger = AuditLogger;
exports.BUILTIN_SCHEMAS = BUILTIN_SCHEMAS;
exports.CapabilityNegotiator = CapabilityNegotiator;
exports.CertificateManager = CertificateManager;
exports.CircuitState = CircuitState;
exports.DHTClient = DHTClient;
exports.DIDManager = DIDManager;
exports.DIDResolver = DIDResolver;
exports.Dashboard = Dashboard;
exports.DashboardError = DashboardError;
exports.DoubleRatchetSession = DoubleRatchetSession;
exports.EncryptedAuditLogger = EncryptedAuditLogger;
exports.KBucket = KBucket;
exports.KnockProtocol = KnockProtocol;
exports.PREKEY_CONFIG = PREKEY_CONFIG;
exports.PROTOCOL_VERSION = PROTOCOL_VERSION;
exports.PrekeyManager = PrekeyManager;
exports.ProtocolSessionManager = ProtocolSessionManager;
exports.RateLimitError = RateLimitError;
exports.RateLimiter = RateLimiter;
exports.SchemaValidator = SchemaValidator;
exports.SequenceTracker = SequenceTracker;
exports.SessionCache = SessionCache;
exports.SessionManager = SessionManager;
exports.SessionState = SessionState;
exports.SessionStateType = SessionStateType;
exports.VERSION = VERSION;
exports.X3DHKeyExchange = X3DHKeyExchange;
exports.compareDistance = compareDistance;
exports.createAmidKey = createAmidKey;
exports.createAuditLogger = createAuditLogger;
exports.createCapabilityKey = createCapabilityKey;
exports.createDHTServiceEndpoint = createDHTServiceEndpoint;
exports.createEncryptedAuditLogger = createEncryptedAuditLogger;
exports.createRelayServiceEndpoint = createRelayServiceEndpoint;
exports.createTrustStore = createTrustStore;
exports.createValidator = createValidator;
exports.deserializeIntentFromJSON = deserializeIntentFromJSON;
exports.deserializePrekeyBundle = deserializePrekeyBundle;
exports.deserializeRatchetHeader = deserializeRatchetHeader;
exports.deserializeX3DHMessage = deserializeX3DHMessage;
exports.generateOneTimePrekeys = generateOneTimePrekeys;
exports.generateSignedPrekey = generateSignedPrekey;
exports.generateX25519Keypair = generateX25519Keypair;
exports.getBucketIndex = getBucketIndex;
exports.hkdf = hkdf;
exports.hkdfSimple = hkdfSimple;
exports.kdfCK = kdfCK;
exports.kdfRK = kdfRK;
exports.parseCertificate = parseCertificate;
exports.parsePEM = parsePEM;
exports.serializeIntentToJSON = serializeIntentToJSON;
exports.serializePrekeyBundle = serializePrekeyBundle;
exports.serializeRatchetHeader = serializeRatchetHeader;
exports.serializeX3DHMessage = serializeX3DHMessage;
exports.toPEM = toPEM;
exports.x25519DH = x25519DH;
exports.xorDistance = xorDistance;
//# sourceMappingURL=index.cjs.map
//# sourceMappingURL=index.cjs.map