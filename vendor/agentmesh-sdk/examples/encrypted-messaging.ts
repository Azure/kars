/**
 * End-to-End Encrypted Messaging Example
 *
 * This example demonstrates the X3DH key exchange and
 * Double Ratchet encryption between two agents.
 */

import {
  Identity,
  MemoryStorage,
  PrekeyManager,
  SessionManager,
} from '@agentmesh/sdk';

async function main() {
  console.log('=== End-to-End Encryption Demo ===\n');

  // Create two agents: Alice and Bob
  const aliceIdentity = await Identity.generate();
  const bobIdentity = await Identity.generate();

  console.log('Alice AMID:', aliceIdentity.amid);
  console.log('Bob AMID:', bobIdentity.amid);
  console.log();

  // Setup storage for each agent
  const aliceStorage = new MemoryStorage();
  const bobStorage = new MemoryStorage();

  // Setup prekey managers
  const alicePrekeyManager = new PrekeyManager(aliceIdentity, aliceStorage);
  const bobPrekeyManager = new PrekeyManager(bobIdentity, bobStorage);

  // Initialize prekeys (Bob's bundle will be used for X3DH)
  await alicePrekeyManager.loadOrInitialize();
  const bobBundle = await bobPrekeyManager.loadOrInitialize();

  console.log('Prekeys initialized');
  console.log('Bob has', bobBundle.oneTimePrekeys.length, 'one-time prekeys');
  console.log();

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

  // Alice initiates session with Bob
  console.log('Alice initiating session with Bob...');
  const { sessionId: aliceSessionId, x3dhMessage } = await aliceSessionManager.initiateSession(
    bobIdentity.amid,
    bobBundle,
    bobIdentity.getSigningPublicKeyRaw()
  );
  console.log('Alice session ID:', aliceSessionId);

  // Bob accepts the session
  console.log('Bob accepting session...');
  const bobSessionId = await bobSessionManager.acceptSession(
    aliceIdentity.amid,
    x3dhMessage
  );
  console.log('Bob session ID:', bobSessionId);

  // Get Bob's ratchet key and activate Alice's session
  const bobRatchetKey = bobSessionManager.getRatchetPublicKey(bobSessionId);
  await aliceSessionManager.activateSession(aliceSessionId, bobRatchetKey!);
  console.log('Sessions established!\n');

  // Exchange encrypted messages
  console.log('=== Message Exchange ===\n');

  // Alice sends to Bob
  const message1 = { text: 'Hello Bob! This is encrypted.', timestamp: Date.now() };
  console.log('Alice sends:', message1.text);

  const envelope1 = await aliceSessionManager.encryptMessage(aliceSessionId, message1);
  console.log('Encrypted envelope size:', JSON.stringify(envelope1).length, 'bytes');

  const decrypted1 = await bobSessionManager.decryptMessage(bobSessionId, envelope1);
  console.log('Bob decrypts:', (decrypted1 as { text: string }).text);
  console.log();

  // Bob replies to Alice
  const message2 = { text: 'Hi Alice! Encryption works!', timestamp: Date.now() };
  console.log('Bob sends:', message2.text);

  const envelope2 = await bobSessionManager.encryptMessage(bobSessionId, message2);
  const decrypted2 = await aliceSessionManager.decryptMessage(aliceSessionId, envelope2);
  console.log('Alice decrypts:', (decrypted2 as { text: string }).text);
  console.log();

  // Multiple message exchange to demonstrate ratchet advancement
  console.log('=== Multiple Messages (Ratchet Advancement) ===\n');

  for (let i = 1; i <= 3; i++) {
    const aliceMsg = { text: `Alice message #${i}`, seq: i };
    const aliceEnv = await aliceSessionManager.encryptMessage(aliceSessionId, aliceMsg);
    const decryptedByBob = await bobSessionManager.decryptMessage(bobSessionId, aliceEnv);
    console.log(`Alice -> Bob: "${(decryptedByBob as { text: string }).text}"`);

    const bobMsg = { text: `Bob reply #${i}`, seq: i };
    const bobEnv = await bobSessionManager.encryptMessage(bobSessionId, bobMsg);
    const decryptedByAlice = await aliceSessionManager.decryptMessage(aliceSessionId, bobEnv);
    console.log(`Bob -> Alice: "${(decryptedByAlice as { text: string }).text}"`);
    console.log();
  }

  // Session stats
  const aliceInfo = aliceSessionManager.getSession(aliceSessionId);
  const bobInfo = bobSessionManager.getSession(bobSessionId);

  console.log('=== Session Statistics ===\n');
  console.log('Alice session:');
  console.log('  Messages sent:', aliceInfo?.messagesSent);
  console.log('  Messages received:', aliceInfo?.messagesReceived);
  console.log();
  console.log('Bob session:');
  console.log('  Messages sent:', bobInfo?.messagesSent);
  console.log('  Messages received:', bobInfo?.messagesReceived);
}

main().catch(console.error);
