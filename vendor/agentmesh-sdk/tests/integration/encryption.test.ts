/**
 * Integration tests for the encryption flow.
 * Tests X3DH key exchange + Double Ratchet messaging between two agents.
 */
import { describe, test, expect, beforeEach } from 'vitest';
import { Identity } from '../../src/identity';
import { MemoryStorage } from '../../src/storage';
import { PrekeyManager, SessionManager } from '../../src/encryption';

describe('Encryption Integration', () => {
  let aliceIdentity: Identity;
  let bobIdentity: Identity;
  let aliceStorage: MemoryStorage;
  let bobStorage: MemoryStorage;

  beforeEach(async () => {
    // Generate identities for Alice and Bob
    aliceIdentity = await Identity.generate();
    bobIdentity = await Identity.generate();
    aliceStorage = new MemoryStorage();
    bobStorage = new MemoryStorage();
  });

  describe('X3DH + Double Ratchet Flow', () => {
    test('should establish encrypted session and exchange messages', async () => {
      // Setup prekey managers
      const alicePrekeyManager = new PrekeyManager(aliceIdentity, aliceStorage);
      const bobPrekeyManager = new PrekeyManager(bobIdentity, bobStorage);

      // Initialize prekeys
      await alicePrekeyManager.loadOrInitialize();
      const bobBundle = await bobPrekeyManager.loadOrInitialize();

      // Setup session managers
      const aliceSessionManager = new SessionManager(
        aliceIdentity,
        aliceStorage,
        alicePrekeyManager
      );
      const bobSessionManager = new SessionManager(
        bobIdentity,
        bobStorage,
        bobPrekeyManager
      );

      // Alice initiates session with Bob using Bob's prekey bundle
      const { sessionId: aliceSessionId, x3dhMessage } = await aliceSessionManager.initiateSession(
        bobIdentity.amid,
        bobBundle,
        bobIdentity.getSigningPublicKeyRaw()
      );

      expect(aliceSessionId).toBeDefined();
      expect(x3dhMessage).toBeDefined();

      // Bob accepts the session using the X3DH message
      const bobSessionId = await bobSessionManager.acceptSession(
        aliceIdentity.amid,
        x3dhMessage
      );

      expect(bobSessionId).toBeDefined();

      // Get Bob's ratchet public key and activate Alice's session
      const bobRatchetKey = bobSessionManager.getRatchetPublicKey(bobSessionId);
      expect(bobRatchetKey).toBeDefined();
      await aliceSessionManager.activateSession(aliceSessionId, bobRatchetKey!);

      // Alice sends a message to Bob
      const message1 = { text: 'Hello Bob!', timestamp: Date.now() };
      const envelope1 = await aliceSessionManager.encryptMessage(aliceSessionId, message1);

      expect(envelope1).toBeDefined();
      expect(envelope1.ciphertext).toBeDefined();

      // Bob decrypts the message
      const decrypted1 = await bobSessionManager.decryptMessage(bobSessionId, envelope1);

      expect(decrypted1).toEqual(message1);

      // Bob replies to Alice
      const message2 = { text: 'Hi Alice!', timestamp: Date.now() };
      const envelope2 = await bobSessionManager.encryptMessage(bobSessionId, message2);

      // Alice decrypts Bob's reply
      const decrypted2 = await aliceSessionManager.decryptMessage(aliceSessionId, envelope2);

      expect(decrypted2).toEqual(message2);
    });

    test('should exchange multiple messages with ratchet advancement', async () => {
      // Setup
      const alicePrekeyManager = new PrekeyManager(aliceIdentity, aliceStorage);
      const bobPrekeyManager = new PrekeyManager(bobIdentity, bobStorage);
      await alicePrekeyManager.loadOrInitialize();
      const bobBundle = await bobPrekeyManager.loadOrInitialize();

      const aliceSessionManager = new SessionManager(
        aliceIdentity,
        aliceStorage,
        alicePrekeyManager
      );
      const bobSessionManager = new SessionManager(
        bobIdentity,
        bobStorage,
        bobPrekeyManager
      );

      // Establish session
      const { sessionId: aliceSessionId, x3dhMessage } = await aliceSessionManager.initiateSession(
        bobIdentity.amid,
        bobBundle,
        bobIdentity.getSigningPublicKeyRaw()
      );
      const bobSessionId = await bobSessionManager.acceptSession(
        aliceIdentity.amid,
        x3dhMessage
      );

      // Activate Alice's session with Bob's ratchet key
      const bobRatchetKey = bobSessionManager.getRatchetPublicKey(bobSessionId);
      await aliceSessionManager.activateSession(aliceSessionId, bobRatchetKey!);

      // Multiple message exchange to test ratchet advancement
      for (let i = 0; i < 5; i++) {
        const aliceMsg = { text: `Alice message ${i}`, seq: i };
        const aliceEnv = await aliceSessionManager.encryptMessage(aliceSessionId, aliceMsg);
        const decryptedByBob = await bobSessionManager.decryptMessage(bobSessionId, aliceEnv);
        expect(decryptedByBob).toEqual(aliceMsg);

        const bobMsg = { text: `Bob message ${i}`, seq: i };
        const bobEnv = await bobSessionManager.encryptMessage(bobSessionId, bobMsg);
        const decryptedByAlice = await aliceSessionManager.decryptMessage(aliceSessionId, bobEnv);
        expect(decryptedByAlice).toEqual(bobMsg);
      }
    });

    test('should handle out-of-order messages', async () => {
      // Setup
      const alicePrekeyManager = new PrekeyManager(aliceIdentity, aliceStorage);
      const bobPrekeyManager = new PrekeyManager(bobIdentity, bobStorage);
      await alicePrekeyManager.loadOrInitialize();
      const bobBundle = await bobPrekeyManager.loadOrInitialize();

      const aliceSessionManager = new SessionManager(
        aliceIdentity,
        aliceStorage,
        alicePrekeyManager
      );
      const bobSessionManager = new SessionManager(
        bobIdentity,
        bobStorage,
        bobPrekeyManager
      );

      // Establish session
      const { sessionId: aliceSessionId, x3dhMessage } = await aliceSessionManager.initiateSession(
        bobIdentity.amid,
        bobBundle,
        bobIdentity.getSigningPublicKeyRaw()
      );
      const bobSessionId = await bobSessionManager.acceptSession(
        aliceIdentity.amid,
        x3dhMessage
      );

      // Activate Alice's session with Bob's ratchet key
      const bobRatchetKey = bobSessionManager.getRatchetPublicKey(bobSessionId);
      await aliceSessionManager.activateSession(aliceSessionId, bobRatchetKey!);

      // Alice sends multiple messages (must be sequential to maintain ratchet state)
      const messages = [
        { text: 'Message 1', seq: 1 },
        { text: 'Message 2', seq: 2 },
        { text: 'Message 3', seq: 3 },
      ];

      const envelopes = [];
      for (const m of messages) {
        envelopes.push(await aliceSessionManager.encryptMessage(aliceSessionId, m));
      }

      // Bob receives them out of order (3, 1, 2)
      const decrypted3 = await bobSessionManager.decryptMessage(bobSessionId, envelopes[2]!);
      expect(decrypted3).toEqual(messages[2]);

      const decrypted1 = await bobSessionManager.decryptMessage(bobSessionId, envelopes[0]!);
      expect(decrypted1).toEqual(messages[0]);

      const decrypted2 = await bobSessionManager.decryptMessage(bobSessionId, envelopes[1]!);
      expect(decrypted2).toEqual(messages[1]);
    });
  });

  describe('Identity and Prekey Integration', () => {
    test('should save and load identity', async () => {
      // Generate and save identity
      const identity = await Identity.generate();
      const storage = new MemoryStorage();

      await identity.save(storage, 'test-identity');

      // Load identity from storage
      const loadedIdentity = await Identity.load(storage, 'test-identity');
      expect(loadedIdentity.amid).toBe(identity.amid);
    });

    test('should create prekey bundle for identity', async () => {
      const identity = await Identity.generate();
      const storage = new MemoryStorage();

      // Create and initialize prekey manager
      const prekeyManager = new PrekeyManager(identity, storage);
      const bundle = await prekeyManager.loadOrInitialize();

      expect(bundle.identityKey).toBeDefined();
      expect(bundle.signedPrekey).toBeDefined();
      expect(bundle.signedPrekeySignature).toBeDefined();
      expect(bundle.oneTimePrekeys).toBeDefined();
      expect(bundle.oneTimePrekeys.length).toBeGreaterThan(0);
    });
  });

  describe('Session Info', () => {
    test('should track session information', async () => {
      // Setup
      const alicePrekeyManager = new PrekeyManager(aliceIdentity, aliceStorage);
      const bobPrekeyManager = new PrekeyManager(bobIdentity, bobStorage);
      await alicePrekeyManager.loadOrInitialize();
      const bobBundle = await bobPrekeyManager.loadOrInitialize();

      const aliceSessionManager = new SessionManager(
        aliceIdentity,
        aliceStorage,
        alicePrekeyManager
      );
      const bobSessionManager = new SessionManager(
        bobIdentity,
        bobStorage,
        bobPrekeyManager
      );

      // Establish session
      const { sessionId: aliceSessionId, x3dhMessage } = await aliceSessionManager.initiateSession(
        bobIdentity.amid,
        bobBundle,
        bobIdentity.getSigningPublicKeyRaw()
      );
      const bobSessionId = await bobSessionManager.acceptSession(
        aliceIdentity.amid,
        x3dhMessage
      );

      // Check session info (using getSession method)
      const aliceInfo = aliceSessionManager.getSession(aliceSessionId);
      const bobInfo = bobSessionManager.getSession(bobSessionId);

      expect(aliceInfo).toBeDefined();
      expect(aliceInfo?.peerAmid).toBe(bobIdentity.amid);
      expect(aliceInfo?.isInitiator).toBe(true);

      expect(bobInfo).toBeDefined();
      expect(bobInfo?.peerAmid).toBe(aliceIdentity.amid);
      expect(bobInfo?.isInitiator).toBe(false);
    });
  });
});
