/**
 * Unit tests for Audit module.
 */
import { describe, test, expect, beforeEach } from 'vitest';
import { AuditLogger, createAuditLogger } from '../../src/audit';

describe('Audit', () => {
  let logger: AuditLogger;

  beforeEach(() => {
    // Use DEBUG minSeverity to ensure all events are stored
    logger = new AuditLogger({
      amid: 'test-amid',
      minSeverity: 'DEBUG',
    });
  });

  describe('AuditLogger', () => {
    test('should create logger with default config', () => {
      const defaultLogger = createAuditLogger('test-amid');
      expect(defaultLogger).toBeDefined();
      expect(defaultLogger.getCount()).toBe(0);
    });

    test('should log event', async () => {
      const event = await logger.log('CONNECTION_ESTABLISHED', 'INFO', 'Connected');
      expect(event.type).toBe('CONNECTION_ESTABLISHED');
      expect(event.severity).toBe('INFO');
      expect(event.message).toBe('Connected');
      expect(event.amid).toBe('test-amid');
      expect(logger.getCount()).toBe(1);
    });

    test('should log event with peer and session', async () => {
      const event = await logger.log('MESSAGE_SENT', 'DEBUG', 'Message sent', {
        peerAmid: 'peer-amid',
        sessionId: 'session-123',
      });

      expect(event.peerAmid).toBe('peer-amid');
      expect(event.sessionId).toBe('session-123');
    });

    test('should log event with metadata', async () => {
      const event = await logger.log('REGISTRY_SEARCH', 'INFO', 'Searched', {
        metadata: { query: 'weather/*', results: 5 },
      });

      expect(event.metadata).toBeDefined();
      expect(event.metadata?.query).toBe('weather/*');
    });

    test('should log event with error', async () => {
      const error = new Error('Test error');
      const event = await logger.log('ERROR', 'ERROR', 'An error occurred', { error });

      expect(event.error).toBeDefined();
      expect(event.error?.name).toBe('Error');
      expect(event.error?.message).toBe('Test error');
    });

    test('should not store events below severity threshold', async () => {
      const customLogger = new AuditLogger({
        amid: 'test-amid',
        minSeverity: 'WARNING',
      });

      await customLogger.log('MESSAGE_SENT', 'DEBUG', 'Debug message');
      await customLogger.log('MESSAGE_SENT', 'INFO', 'Info message');
      await customLogger.log('WARNING', 'WARNING', 'Warning message');

      expect(customLogger.getCount()).toBe(1);
    });

    test('should query events by type', async () => {
      await logger.log('CONNECTION_ESTABLISHED', 'INFO', 'Connected');
      await logger.log('MESSAGE_SENT', 'DEBUG', 'Sent 1');
      await logger.log('MESSAGE_SENT', 'DEBUG', 'Sent 2');
      await logger.log('CONNECTION_LOST', 'INFO', 'Disconnected');

      const sent = logger.getByType('MESSAGE_SENT');
      expect(sent.length).toBe(2);
    });

    test('should query events by peer', async () => {
      await logger.log('MESSAGE_SENT', 'DEBUG', 'Sent 1', { peerAmid: 'peer-1' });
      await logger.log('MESSAGE_SENT', 'DEBUG', 'Sent 2', { peerAmid: 'peer-1' });
      await logger.log('MESSAGE_SENT', 'DEBUG', 'Sent 3', { peerAmid: 'peer-2' });

      const peer1 = logger.getByPeer('peer-1');
      expect(peer1.length).toBe(2);
    });

    test('should query events by session', async () => {
      await logger.log('MESSAGE_SENT', 'DEBUG', 'Sent 1', { sessionId: 'session-1' });
      await logger.log('MESSAGE_RECEIVED', 'DEBUG', 'Received', { sessionId: 'session-1' });
      await logger.log('MESSAGE_SENT', 'DEBUG', 'Sent 2', { sessionId: 'session-2' });

      const session1 = logger.getBySession('session-1');
      expect(session1.length).toBe(2);
    });

    test('should get error events', async () => {
      await logger.log('CONNECTION_ESTABLISHED', 'INFO', 'Connected');
      await logger.log('ERROR', 'ERROR', 'Error 1');
      await logger.log('ERROR', 'CRITICAL', 'Error 2');

      const errors = logger.getErrors();
      expect(errors.length).toBe(2);
    });

    test('should get recent events', async () => {
      for (let i = 0; i < 20; i++) {
        await logger.log('MESSAGE_SENT', 'INFO', `Message ${i}`);
      }

      const recent = logger.getRecent(5);
      expect(recent.length).toBe(5);
    });

    test('should get statistics', async () => {
      await logger.log('CONNECTION_ESTABLISHED', 'INFO', 'Connected');
      await logger.log('MESSAGE_SENT', 'DEBUG', 'Sent');
      await logger.log('MESSAGE_SENT', 'DEBUG', 'Sent');
      await logger.log('ERROR', 'ERROR', 'Error');

      const stats = logger.getStats();
      expect(stats.total).toBe(4);
      expect(stats.byType['MESSAGE_SENT']).toBe(2);
      expect(stats.bySeverity['ERROR']).toBe(1);
    });

    test('should clear events', async () => {
      await logger.log('CONNECTION_ESTABLISHED', 'INFO', 'Connected');
      await logger.log('MESSAGE_SENT', 'DEBUG', 'Sent');

      logger.clear();
      expect(logger.getCount()).toBe(0);
    });

    test('should export and import events', async () => {
      await logger.log('CONNECTION_ESTABLISHED', 'INFO', 'Connected');
      await logger.log('MESSAGE_SENT', 'DEBUG', 'Sent');

      const json = logger.export();
      expect(json).toBeDefined();

      const newLogger = new AuditLogger({ amid: 'test-amid', minSeverity: 'DEBUG' });
      const imported = newLogger.import(json);
      expect(imported).toBe(2);
      expect(newLogger.getCount()).toBe(2);
    });

    test('should query with time range', async () => {
      const now = new Date();
      await logger.log('MESSAGE_SENT', 'INFO', 'Sent 1');
      await logger.log('MESSAGE_SENT', 'INFO', 'Sent 2');

      const results = logger.query({
        startTime: new Date(now.getTime() - 1000),
        endTime: new Date(now.getTime() + 1000),
      });

      expect(results.length).toBe(2);
    });

    test('should query with pagination', async () => {
      for (let i = 0; i < 10; i++) {
        await logger.log('MESSAGE_SENT', 'INFO', `Message ${i}`);
      }

      const page1 = logger.query({ limit: 3, offset: 0, order: 'asc' });
      const page2 = logger.query({ limit: 3, offset: 3, order: 'asc' });

      expect(page1.length).toBe(3);
      expect(page2.length).toBe(3);
      expect(page1[0]?.message).toBe('Message 0');
      expect(page2[0]?.message).toBe('Message 3');
    });

    test('convenience methods should log correctly', async () => {
      await logger.logIdentityCreated({ algorithm: 'Ed25519' });
      await logger.logSessionInitiated('peer-amid', 'session-123');
      await logger.logMessageSent('peer-amid', 'session-123');
      await logger.logMessageReceived('peer-amid', 'session-123');
      await logger.logError('Test error', new Error('Failed'));
      await logger.logWarning('Test warning');

      const stats = logger.getStats();
      expect(stats.total).toBe(6);
    });

    test('should respect max memory events limit', async () => {
      const smallLogger = new AuditLogger({
        amid: 'test-amid',
        maxMemoryEvents: 5,
        minSeverity: 'DEBUG',
      });

      for (let i = 0; i < 10; i++) {
        await smallLogger.log('MESSAGE_SENT', 'INFO', `Message ${i}`);
      }

      expect(smallLogger.getCount()).toBe(5);
      // Oldest events should be removed
      const events = smallLogger.query({ order: 'asc' });
      expect(events[0]?.message).toBe('Message 5');
    });
  });
});
