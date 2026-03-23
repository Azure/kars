/**
 * Unit tests for Session/KNOCK protocol module.
 */
import { describe, test, expect, beforeEach } from 'vitest';
import {
  KnockProtocol,
  ProtocolSessionManager,
  SessionStateType,
  serializeIntentToJSON,
  deserializeIntentFromJSON,
} from '../../src/session';
import { Identity } from '../../src/identity';
import { Policy } from '../../src/config';

describe('Session/KNOCK Protocol', () => {
  let identity: Identity;

  beforeEach(async () => {
    identity = await Identity.generate();
  });

  describe('Intent Serialization', () => {
    test('should serialize intent to JSON', () => {
      const intent = {
        capability: 'weather/forecast',
        action: 'query',
      };

      const json = serializeIntentToJSON(intent);
      expect(json.capability).toBe('weather/forecast');
      expect(json.action).toBe('query');
    });

    test('should deserialize intent from JSON', () => {
      const json = {
        capability: 'travel/flights',
        action: 'book',
        parameters: { from: 'NYC', to: 'LAX' },
      };

      const intent = deserializeIntentFromJSON(json);
      expect(intent.capability).toBe('travel/flights');
      expect(intent.action).toBe('book');
    });
  });

  describe('KnockProtocol', () => {
    test('should create protocol instance', () => {
      const protocol = new KnockProtocol(identity);
      expect(protocol).toBeDefined();
    });

    test('should set and use policy', () => {
      const protocol = new KnockProtocol(identity);
      const policy = Policy.verified();
      protocol.setPolicy(policy);
      // Policy is set internally
    });

    test('should create KNOCK message', async () => {
      const protocol = new KnockProtocol(identity);
      const knock = await protocol.createKnock('recipient-amid', {
        type: 'one-shot',
        ttl: 300,
        intent: {
          capability: 'weather/forecast',
          action: 'query',
        },
      });

      expect(knock.from).toBe(identity.amid);
      expect(knock.to).toBe('recipient-amid');
      expect(knock.request.intent.capability).toBe('weather/forecast');
      expect(knock.signature).toBeDefined();
      expect(knock.nonce).toBeDefined();
    });

    test('should validate own KNOCK message structure', async () => {
      // Create a recipient to validate the message
      const recipient = await Identity.generate();
      const recipientProtocol = new KnockProtocol(recipient);

      const senderProtocol = new KnockProtocol(identity);
      const knock = await senderProtocol.createKnock(recipient.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: {
          capability: 'weather/forecast',
          action: 'query',
        },
      });

      const result = await recipientProtocol.validateKnock(knock);
      expect(result.valid).toBe(true);
    });

    test('should reject knock with wrong recipient', async () => {
      const protocol = new KnockProtocol(identity);
      const knock = await protocol.createKnock('someone-else', {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      });

      // Validating as if we're a different identity
      const result = await protocol.validateKnock(knock);
      expect(result.valid).toBe(false);
      expect(result.error).toContain('not addressed to us');
    });

    test('should create ACCEPT response', async () => {
      const recipient = await Identity.generate();
      const recipientProtocol = new KnockProtocol(recipient);

      const senderProtocol = new KnockProtocol(identity);
      const knock = await senderProtocol.createKnock(recipient.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      });

      const response = await recipientProtocol.createAcceptResponse(knock);
      expect(response.type).toBe('ACCEPT');
      expect(response.knockNonce).toBe(knock.nonce);
      expect(response.sessionId).toBeDefined();
    });

    test('should create REJECT response', async () => {
      const recipient = await Identity.generate();
      const recipientProtocol = new KnockProtocol(recipient);

      const senderProtocol = new KnockProtocol(identity);
      const knock = await senderProtocol.createKnock(recipient.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      });

      const response = await recipientProtocol.createRejectResponse(knock, 'Not authorized');
      expect(response.type).toBe('REJECT');
      expect(response.reason).toBe('Not authorized');
      expect(response.knockNonce).toBe(knock.nonce);
    });

    test('should evaluate KNOCK against policy', async () => {
      const recipient = await Identity.generate();
      const recipientProtocol = new KnockProtocol(recipient);
      recipientProtocol.setPolicy(Policy.permissive());

      const senderProtocol = new KnockProtocol(identity);
      const knock = await senderProtocol.createKnock(recipient.amid, {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'weather/forecast', action: 'query' },
      });

      const result = await recipientProtocol.evaluateKnock(knock, {
        publicKey: identity.getSigningPublicKeyRaw(),
        tier: 'verified',
        reputation: 0.8,
      });

      expect(result.allowed).toBe(true);
    });
  });

  describe('ProtocolSessionManager', () => {
    test('should create session manager', () => {
      const manager = new ProtocolSessionManager();
      expect(manager).toBeDefined();
    });

    test('should create session', () => {
      const manager = new ProtocolSessionManager();
      const session = manager.createSession('peer-amid', {
        type: 'one-shot',
        ttl: 300,
        intent: {
          capability: 'weather/forecast',
          action: 'query',
        },
      }, true);

      expect(session.remoteAmid).toBe('peer-amid');
      expect(session.state).toBe(SessionStateType.ACTIVE);
      expect(session.isInitiator).toBe(true);
    });

    test('should get session by ID', () => {
      const manager = new ProtocolSessionManager();
      const session = manager.createSession('peer-amid', {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      }, true);

      const found = manager.getSession(session.id);
      expect(found).toBeDefined();
      expect(found?.id).toBe(session.id);
    });

    test('should update session state', () => {
      const manager = new ProtocolSessionManager();
      const session = manager.createSession('peer-amid', {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      }, true);

      manager.updateSessionState(session.id, SessionStateType.CLOSED);
      const updated = manager.getSession(session.id);
      expect(updated?.state).toBe(SessionStateType.CLOSED);
    });

    test('should get sessions for peer', () => {
      const manager = new ProtocolSessionManager();
      manager.createSession('peer-1', {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      }, true);
      manager.createSession('peer-1', {
        type: 'streaming',
        ttl: 600,
        intent: { capability: 'test2', action: 'test' },
      }, true);
      manager.createSession('peer-2', {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      }, true);

      const peer1Sessions = manager.getSessionsForPeer('peer-1');
      expect(peer1Sessions.length).toBe(2);
    });

    test('should get active sessions', () => {
      const manager = new ProtocolSessionManager();
      const session1 = manager.createSession('peer-1', {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      }, true);

      const session2 = manager.createSession('peer-2', {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      }, true);
      manager.updateSessionState(session2.id, SessionStateType.CLOSED);

      const active = manager.getActiveSessions();
      expect(active.length).toBe(1);
      expect(active[0]?.id).toBe(session1.id);
    });

    test('should close session', () => {
      const manager = new ProtocolSessionManager();
      const session = manager.createSession('peer-amid', {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      }, true);

      manager.closeSession(session.id);
      const closed = manager.getSession(session.id);
      expect(closed?.state).toBe(SessionStateType.CLOSED);
    });

    test('should record message sent', () => {
      const manager = new ProtocolSessionManager();
      const session = manager.createSession('peer-amid', {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      }, true);

      manager.recordMessageSent(session.id);
      const updated = manager.getSession(session.id);
      expect(updated?.messagesSent).toBe(1);
    });

    test('should record message received', () => {
      const manager = new ProtocolSessionManager();
      const session = manager.createSession('peer-amid', {
        type: 'one-shot',
        ttl: 300,
        intent: { capability: 'test', action: 'test' },
      }, true);

      manager.recordMessageReceived(session.id);
      const updated = manager.getSession(session.id);
      expect(updated?.messagesReceived).toBe(1);
    });
  });
});
