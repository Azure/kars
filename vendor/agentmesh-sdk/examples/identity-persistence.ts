/**
 * Identity Persistence Example
 *
 * This example demonstrates saving and loading agent identities,
 * and working with different storage backends.
 */

import {
  Identity,
  MemoryStorage,
  FileStorage,
} from '@agentmesh/sdk';

async function main() {
  console.log('=== Identity Persistence Demo ===\n');

  // Create a new identity
  const identity = await Identity.generate();

  console.log('Generated new identity:');
  console.log('  AMID:', identity.amid);
  console.log('  Signing Public Key (B64):', identity.signingPublicKeyB64.substring(0, 40) + '...');
  console.log('  Exchange Public Key (B64):', identity.exchangePublicKeyB64.substring(0, 40) + '...');
  console.log();

  // Demonstrate memory storage
  console.log('=== Memory Storage ===\n');

  const memoryStorage = new MemoryStorage();

  // Save identity to memory storage
  await identity.save(memoryStorage, 'my-agent');
  console.log('Identity saved to memory storage');

  // Load identity from memory storage
  const loadedFromMemory = await Identity.load(memoryStorage, 'my-agent');
  console.log('Identity loaded from memory storage');
  console.log('  AMID matches:', loadedFromMemory.amid === identity.amid);
  console.log();

  // Demonstrate file storage
  console.log('=== File Storage ===\n');

  // Create file storage in temp directory
  const tempDir = '/tmp/agentmesh-demo';
  const fileStorage = new FileStorage(tempDir);

  // Save identity to file storage
  await identity.save(fileStorage, 'persistent-agent');
  console.log(`Identity saved to file storage (${tempDir})`);

  // Load identity from file storage
  const loadedFromFile = await Identity.load(fileStorage, 'persistent-agent');
  console.log('Identity loaded from file storage');
  console.log('  AMID matches:', loadedFromFile.amid === identity.amid);
  console.log();

  // Demonstrate signing
  console.log('=== Signing and Verification ===\n');

  const message = new TextEncoder().encode('Hello, AgentMesh!');

  // Sign message
  const signature = await identity.sign(message);
  console.log('Message signed');
  console.log('  Signature length:', signature.length, 'bytes');

  // Verify signature
  const isValid = await Identity.verifySignature(
    message,
    signature,
    identity.getSigningPublicKeyRaw()
  );
  console.log('  Signature valid:', isValid);

  // Verify with wrong key fails
  const otherIdentity = await Identity.generate();
  const isInvalid = await Identity.verifySignature(
    message,
    signature,
    otherIdentity.getSigningPublicKeyRaw()
  );
  console.log('  Wrong key verification:', isInvalid ? 'VALID (unexpected!)' : 'INVALID (correct)');
  console.log();

  // Demonstrate timestamp signing
  console.log('=== Timestamp Signing ===\n');

  const [timestamp, timestampSig] = await identity.signTimestamp();
  console.log('Signed timestamp:', timestamp);
  console.log('Signature (B64):', timestampSig.substring(0, 40) + '...');
  console.log();

  // Demonstrate public info export
  console.log('=== Public Info Export ===\n');

  const publicInfo = identity.toPublicInfo();
  console.log('Public info (safe to share):');
  console.log(JSON.stringify(publicInfo, null, 2));
  console.log();

  // Demonstrate key rotation
  console.log('=== Key Rotation ===\n');

  const oldAmid = identity.amid;
  console.log('Old AMID:', oldAmid);

  // Note: rotateKeys creates a NEW identity, doesn't mutate the old one
  const newIdentity = await identity.rotateKeys();
  console.log('New AMID:', newIdentity.amid);
  console.log('AMIDs are different:', oldAmid !== newIdentity.amid);
  console.log();

  // Storage listing
  console.log('=== Storage Contents ===\n');

  const memoryFiles = await memoryStorage.list('');
  console.log('Memory storage files:', memoryFiles);

  const fileStorageFiles = await fileStorage.list('');
  console.log('File storage files:', fileStorageFiles);
}

main().catch(console.error);
