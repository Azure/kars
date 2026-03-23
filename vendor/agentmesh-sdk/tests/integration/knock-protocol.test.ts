/**
 * Integration tests for the KNOCK protocol flow.
 * Tests session establishment between two agents.
 */
import { describe, test, expect, beforeEach } from 'vitest';
import { Identity } from '../../src/identity';
import { KnockProtocol, ProtocolSessionManager, SessionStateType } from '../../src/session';
import { Policy, Tier } from '../../src/config';
import { createAuditLogger } from '../../src/audit';

describe('KNOCK Protocol Integration', () => {
  let aliceIdentity: Identity;
  let bobIdentity: Identity;
  let aliceProtocol: KnockProtocol;
  let bobProtocol: KnockProtocol;

  beforeEach(async () => {
    aliceIdentity = await Identity.generate();
    bobIdentity = await Identity.generate();
    aliceProtocol = new KnockProtocol(aliceIdentity);
    bobProtocol = new KnockProtocol(bobIdentity);
  });

  describe('Session Establishment Flow', () => {
    test('should complete full KNOCK handshake', async () => {
      // Alice creates a KNOCK request to Bob
      const knock = await aliceProtocol.createKnock(bobIdentity.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: {
          capability: 'weather/forecast',
          action: 'query',
        },
      });

      expect(knock.from).toBe(aliceIdentity.amid);
      expect(knock.to).toBe(bobIdentity.amid);

      // Bob validates the KNOCK
      const validation = await bobProtocol.validateKnock(knock);
      expect(validation.valid).toBe(true);

      // Bob evaluates against policy (permissive)
      bobProtocol.setPolicy(Policy.permissive());
      const evaluation = await bobProtocol.evaluateKnock(knock, {
        publicKey: aliceIdentity.getSigningPublicKeyRaw(),
        tier: 'verified',
        reputation: 0.8,
      });
      expect(evaluation.allowed).toBe(true);

      // Bob accepts the KNOCK
      const response = await bobProtocol.createAcceptResponse(knock);
      expect(response.type).toBe('ACCEPT');
      expect(response.sessionId).toBeDefined();

      // Alice validates the response
      const responseValid = await aliceProtocol.validateResponse(response, knock);
      expect(responseValid.valid).toBe(true);
    });

    test('should reject KNOCK based on policy', async () => {
      // Bob requires organization tier
      bobProtocol.setPolicy(Policy.organization());

      const knock = await aliceProtocol.createKnock(bobIdentity.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: {
          capability: 'admin/settings',
          action: 'modify',
        },
      });

      // Validate structure
      const validation = await bobProtocol.validateKnock(knock);
      expect(validation.valid).toBe(true);

      // Evaluate against policy - should fail for anonymous tier
      const evaluation = await bobProtocol.evaluateKnock(knock, {
        publicKey: aliceIdentity.getSigningPublicKeyRaw(),
        tier: 'anonymous',
        reputation: 0.3,
      });
      expect(evaluation.allowed).toBe(false);

      // Bob rejects the KNOCK
      const response = await bobProtocol.createRejectResponse(knock, evaluation.reason || 'Policy rejected');
      expect(response.type).toBe('REJECT');
      expect(response.reason).toBeDefined();
    });

    test('should detect replay attacks', async () => {
      const knock = await aliceProtocol.createKnock(bobIdentity.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      });

      // First validation should succeed
      const firstValidation = await bobProtocol.validateKnock(knock);
      expect(firstValidation.valid).toBe(true);

      // Second validation of same KNOCK should fail (replay detection)
      const replayValidation = await bobProtocol.validateKnock(knock);
      expect(replayValidation.valid).toBe(false);
      expect(replayValidation.error).toContain('Replay');
    });
  });

  describe('Session Management', () => {
    test('should manage multiple sessions', () => {
      const manager = new ProtocolSessionManager();

      // Create sessions with different peers
      const session1 = manager.createSession('peer-1', {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'weather/forecast', action: 'query' },
      }, true);

      const session2 = manager.createSession('peer-2', {
        type: 'streaming',
        ttl: 600,
        intent: { capability: 'notifications/subscribe', action: 'subscribe' },
      }, true);

      const session3 = manager.createSession('peer-1', {
        type: 'persistent',
        ttl: 3600,
        intent: { capability: 'chat/messages', action: 'send' },
      }, false);

      // Verify all sessions created
      expect(manager.getActiveSessions().length).toBe(3);

      // Get sessions for specific peer
      const peer1Sessions = manager.getSessionsForPeer('peer-1');
      expect(peer1Sessions.length).toBe(2);

      // Track message counts
      manager.recordMessageSent(session1.id);
      manager.recordMessageSent(session1.id);
      manager.recordMessageReceived(session1.id);

      const updated = manager.getSession(session1.id);
      expect(updated?.messagesSent).toBe(2);
      expect(updated?.messagesReceived).toBe(1);

      // Close a session
      manager.closeSession(session2.id);
      expect(manager.getActiveSessions().length).toBe(2);
      expect(manager.getSession(session2.id)?.state).toBe(SessionStateType.CLOSED);
    });

    test('should handle session expiration state', () => {
      const manager = new ProtocolSessionManager();

      const session = manager.createSession('peer-1', {
        type: 'one-shot',
        ttl: 1, // 1 second TTL
        intent: { capability: 'test', action: 'test' },
      }, true);

      expect(session.state).toBe(SessionStateType.ACTIVE);

      // Manually expire the session
      manager.updateSessionState(session.id, SessionStateType.EXPIRED);

      const expired = manager.getSession(session.id);
      expect(expired?.state).toBe(SessionStateType.EXPIRED);

      // Should not appear in active sessions
      expect(manager.getActiveSessions().length).toBe(0);
    });
  });

  describe('Audit Integration', () => {
    test('should log session events', async () => {
      const logger = createAuditLogger(bobIdentity.amid);

      // Alice sends KNOCK
      const knock = await aliceProtocol.createKnock(bobIdentity.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      });

      // Bob receives and logs
      await logger.log('KNOCK_RECEIVED', 'INFO', 'Received KNOCK', {
        peerAmid: knock.from,
        metadata: { intent: knock.request.intent.capability },
      });

      // Validate
      const validation = await bobProtocol.validateKnock(knock);
      if (validation.valid) {
        await logger.log('KNOCK_VALIDATED', 'INFO', 'KNOCK validated successfully', {
          peerAmid: knock.from,
        });
      }

      // Accept and log
      const response = await bobProtocol.createAcceptResponse(knock);
      await logger.logSessionInitiated(knock.from, response.sessionId!);

      // Verify audit trail
      const events = logger.query({ type: 'SESSION_INITIATED' });
      expect(events.length).toBe(1);
      expect(events[0]?.peerAmid).toBe(aliceIdentity.amid);

      const allEvents = logger.query({});
      expect(allEvents.length).toBe(3);
    });
  });

  describe('Policy Configurations', () => {
    test('should handle blocked AMIDs', async () => {
      const policy = new Policy({
        blockedAmids: [aliceIdentity.amid],
      });
      bobProtocol.setPolicy(policy);

      const knock = await aliceProtocol.createKnock(bobIdentity.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      });

      const evaluation = await bobProtocol.evaluateKnock(knock, {
        publicKey: aliceIdentity.getSigningPublicKeyRaw(),
        tier: 'verified',
        reputation: 1.0,
      });

      expect(evaluation.allowed).toBe(false);
      expect(evaluation.reason).toContain('blocked');
    });

    test('should handle intent restrictions', async () => {
      // Policy uses exact matching for intent categories
      const policy = new Policy({
        allowedIntents: ['public/data', 'public/list'],
        blockedIntents: ['admin/settings'],
      });
      bobProtocol.setPolicy(policy);

      // Allowed intent (exact match)
      const knockAllowed = await aliceProtocol.createKnock(bobIdentity.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'public/data', action: 'read' },
      });

      const evalAllowed = await bobProtocol.evaluateKnock(knockAllowed, {
        publicKey: aliceIdentity.getSigningPublicKeyRaw(),
        tier: 'verified',
        reputation: 0.8,
      });
      expect(evalAllowed.allowed).toBe(true);

      // Blocked intent (exact match)
      const knockBlocked = await aliceProtocol.createKnock(bobIdentity.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'admin/settings', action: 'modify' },
      });

      const evalBlocked = await bobProtocol.evaluateKnock(knockBlocked, {
        publicKey: aliceIdentity.getSigningPublicKeyRaw(),
        tier: 'verified',
        reputation: 0.8,
      });
      expect(evalBlocked.allowed).toBe(false);
    });

    test('should enforce TTL limits', async () => {
      const policy = new Policy({
        maxSessionTtl: 60, // 1 minute max
      });
      bobProtocol.setPolicy(policy);

      const knock = await aliceProtocol.createKnock(bobIdentity.amid, {
        type: 'persistent',
        ttl: 3600, // 1 hour requested
        intent: { capability: 'test', action: 'test' },
      });

      const evaluation = await bobProtocol.evaluateKnock(knock, {
        publicKey: aliceIdentity.getSigningPublicKeyRaw(),
        tier: 'verified',
        reputation: 0.8,
      });

      expect(evaluation.allowed).toBe(false);
      expect(evaluation.reason).toContain('TTL');
    });
  });
});
