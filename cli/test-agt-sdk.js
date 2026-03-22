const sdk = require('@agentmesh/sdk');

async function test() {
  // Test Identity generation (Ed25519 + X25519)
  const id = await sdk.Identity.generate();
  console.log('=== Identity ===');
  console.log('AMID:', id.amid);
  console.log('publicKey type:', typeof id.publicKey);
  console.log('Has sign method:', typeof id.sign === 'function');

  // Test Policy
  const policy = new sdk.Policy([
    { action: 'file_read', effect: 'allow' },
    { action: 'shell:rm', effect: 'deny' },
  ]);
  console.log('\n=== Policy ===');
  console.log('file_read:', JSON.stringify(policy.evaluate('file_read')));
  console.log('shell:rm:', JSON.stringify(policy.evaluate('shell:rm')));
  console.log('unknown:', JSON.stringify(policy.evaluate('unknown_action')));

  // Test TrustStore
  const trust = sdk.createTrustStore();
  console.log('\n=== TrustStore ===');
  console.log('Methods:', Object.keys(trust).join(', '));

  // Test AuditLogger
  const audit = sdk.createAuditLogger();
  console.log('\n=== AuditLogger ===');
  console.log('Methods:', Object.keys(audit).join(', '));

  // Test AgentMeshClient (without connecting)
  const client = new sdk.AgentMeshClient(id, {
    storage: new sdk.MemoryStorage(),
    registryUrl: 'http://localhost:9999/fake',
    relayUrl: 'ws://localhost:9999/fake',
  });
  console.log('\n=== AgentMeshClient ===');
  const methods = Object.getOwnPropertyNames(Object.getPrototypeOf(client))
    .filter(m => m !== 'constructor');
  console.log('Methods:', methods.join(', '));
}

test().catch(e => console.error('ERR:', e.message));
