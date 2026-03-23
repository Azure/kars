/**
 * KNOCK Protocol Example
 *
 * This example demonstrates the session establishment protocol
 * including policy evaluation and session management.
 */

import {
  Identity,
  KnockProtocol,
  ProtocolSessionManager,
  Policy,
  SessionStateType,
} from '@agentmesh/sdk';

async function main() {
  console.log('=== KNOCK Protocol Demo ===\n');

  // Create two agents
  const aliceIdentity = await Identity.generate();
  const bobIdentity = await Identity.generate();

  console.log('Alice AMID:', aliceIdentity.amid);
  console.log('Bob AMID:', bobIdentity.amid);
  console.log();

  // Setup KNOCK protocols
  const aliceProtocol = new KnockProtocol(aliceIdentity);
  const bobProtocol = new KnockProtocol(bobIdentity);

  // Bob sets a policy requiring verified tier
  bobProtocol.setPolicy(Policy.verified());

  // Session managers
  const aliceSessionManager = new ProtocolSessionManager();
  const bobSessionManager = new ProtocolSessionManager();

  console.log('=== Scenario 1: Successful Connection ===\n');

  // Alice creates a KNOCK request
  const knock1 = await aliceProtocol.createKnock(bobIdentity.amid, {
    type: 'one-shot',
    ttl: 300,
    intent: {
      capability: 'weather/forecast',
      action: 'query',
    },
  });

  console.log('Alice sends KNOCK:');
  console.log('  From:', knock1.from);
  console.log('  To:', knock1.to);
  console.log('  Intent:', knock1.request.intent.capability);
  console.log('  TTL:', knock1.request.ttl, 'seconds');
  console.log();

  // Bob validates the KNOCK
  const validation1 = await bobProtocol.validateKnock(knock1);
  console.log('Bob validates KNOCK:', validation1.valid ? 'VALID' : 'INVALID');

  if (validation1.valid) {
    // Bob evaluates against policy
    const evaluation1 = await bobProtocol.evaluateKnock(knock1, {
      publicKey: aliceIdentity.getSigningPublicKeyRaw(),
      tier: 'verified', // Alice is verified
      reputation: 0.85,
    });

    console.log('Policy evaluation:', evaluation1.allowed ? 'ALLOWED' : 'REJECTED');

    if (evaluation1.allowed) {
      // Bob accepts
      const response1 = await bobProtocol.createAcceptResponse(knock1);
      console.log('Bob responds: ACCEPT');
      console.log('  Session ID:', response1.sessionId);

      // Both create session records
      const aliceSession = aliceSessionManager.createSession(bobIdentity.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'weather/forecast', action: 'query' },
      }, true);

      const bobSession = bobSessionManager.createSession(aliceIdentity.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'weather/forecast', action: 'query' },
      }, false);

      console.log();
      console.log('Sessions created:');
      console.log('  Alice session:', aliceSession.id, '(initiator)');
      console.log('  Bob session:', bobSession.id, '(responder)');
    }
  }

  console.log();
  console.log('=== Scenario 2: Policy Rejection ===\n');

  // Alice tries to access admin capability
  const knock2 = await aliceProtocol.createKnock(bobIdentity.amid, {
    type: 'persistent',
    ttl: 3600,
    intent: {
      capability: 'admin/settings',
      action: 'modify',
    },
  });

  console.log('Alice sends KNOCK for admin access');

  // Bob uses organization policy for admin
  bobProtocol.setPolicy(Policy.organization());

  const evaluation2 = await bobProtocol.evaluateKnock(knock2, {
    publicKey: aliceIdentity.getSigningPublicKeyRaw(),
    tier: 'verified', // Alice is only verified, not organization
    reputation: 0.85,
  });

  console.log('Policy evaluation:', evaluation2.allowed ? 'ALLOWED' : 'REJECTED');
  if (!evaluation2.allowed) {
    console.log('  Reason:', evaluation2.reason);

    // Bob rejects
    const reject = await bobProtocol.createRejectResponse(knock2, evaluation2.reason || 'Policy rejected');
    console.log('Bob responds: REJECT');
    console.log('  Reason:', reject.reason);
  }

  console.log();
  console.log('=== Scenario 3: Blocked AMID ===\n');

  // Bob blocks Alice
  bobProtocol.setPolicy(new Policy({
    blockedAmids: [aliceIdentity.amid],
  }));

  const knock3 = await aliceProtocol.createKnock(bobIdentity.amid, {
    type: 'one-shot',
    ttl: 300,
    intent: { capability: 'public/data', action: 'read' },
  });

  console.log('Alice sends KNOCK (but is blocked)');

  const evaluation3 = await bobProtocol.evaluateKnock(knock3, {
    publicKey: aliceIdentity.getSigningPublicKeyRaw(),
    tier: 'verified',
    reputation: 1.0, // Perfect reputation doesn't matter
  });

  console.log('Policy evaluation:', evaluation3.allowed ? 'ALLOWED' : 'REJECTED');
  console.log('  Reason:', evaluation3.reason);

  console.log();
  console.log('=== Session Management ===\n');

  // Demonstrate session lifecycle
  console.log('Active sessions for Alice:', aliceSessionManager.getActiveSessions().length);
  console.log('Active sessions for Bob:', bobSessionManager.getActiveSessions().length);

  // Close a session
  const aliceSessions = aliceSessionManager.getActiveSessions();
  if (aliceSessions.length > 0) {
    aliceSessionManager.closeSession(aliceSessions[0]!.id);
    console.log();
    console.log('Alice closed session:', aliceSessions[0]!.id);
    console.log('Active sessions after close:', aliceSessionManager.getActiveSessions().length);
  }
}

main().catch(console.error);
