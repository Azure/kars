/**
 * Unit tests for Schemas module.
 */
import { describe, test, expect } from 'vitest';
import {
  SchemaValidator,
  CapabilityNegotiator,
  SequenceTracker,
  createValidator,
  BUILTIN_SCHEMAS,
} from '../../src/schemas';

describe('Schemas', () => {
  describe('SchemaValidator', () => {
    test('should register and list schemas', () => {
      const validator = new SchemaValidator();
      validator.register('schema1', { type: 'object' });
      validator.register('schema2', { type: 'string' });

      const schemas = validator.getRegisteredSchemas();
      expect(schemas).toContain('schema1');
      expect(schemas).toContain('schema2');
    });

    test('should get schema by name', () => {
      const validator = new SchemaValidator();
      const schema = { type: 'object', properties: { x: { type: 'number' } } };
      validator.register('mySchema', schema);

      const retrieved = validator.get('mySchema');
      expect(retrieved).toEqual(schema);
    });

    test('should handle unknown schema ID', () => {
      const validator = new SchemaValidator();
      const result = validator.validateById({ data: 'test' }, 'unknown');
      expect(result.valid).toBe(false);
    });

    test('should validate string constraints', () => {
      const validator = new SchemaValidator();
      const schema = {
        type: 'string',
        minLength: 3,
        maxLength: 10,
      };

      expect(validator.validate('hello', schema).valid).toBe(true);
      expect(validator.validate('hi', schema).valid).toBe(false);
      expect(validator.validate('this is too long', schema).valid).toBe(false);
    });

    test('should reject missing required properties', () => {
      const validator = new SchemaValidator();
      const schema = {
        type: 'object',
        properties: {
          name: { type: 'string' },
        },
        required: ['name'],
      };

      // Missing required 'name' property
      const result = validator.validate({ age: 30 }, schema);
      expect(result.errors.some(e => e.includes('name'))).toBe(true);
    });

    test('should clear schemas', () => {
      const validator = new SchemaValidator();
      validator.register('test', { type: 'string' });
      expect(validator.getRegisteredSchemas().length).toBe(1);

      validator.clear();
      expect(validator.getRegisteredSchemas().length).toBe(0);
    });
  });

  describe('CapabilityNegotiator', () => {
    test('should register and match exact capability', () => {
      const negotiator = new CapabilityNegotiator();
      negotiator.register({
        id: 'weather/forecast',
        name: 'Weather Forecast',
        description: 'Weather forecasts',
        version: '1.0',
        actions: ['query'],
      });

      const matches = negotiator.match('weather/forecast');
      expect(matches.length).toBe(1);
      expect(matches[0]?.exact).toBe(true);
    });

    test('should match wildcard capabilities', () => {
      const negotiator = new CapabilityNegotiator();
      negotiator.register({
        id: 'weather/forecast',
        name: 'Weather Forecast',
        description: 'Forecasts',
        version: '1.0',
        actions: ['query'],
      });
      negotiator.register({
        id: 'weather/alerts',
        name: 'Weather Alerts',
        description: 'Alerts',
        version: '1.0',
        actions: ['subscribe'],
      });

      const matches = negotiator.match('weather/*');
      expect(matches.length).toBe(2);
      expect(matches.every(m => m.wildcard)).toBe(true);
    });

    test('should check action support', () => {
      const negotiator = new CapabilityNegotiator();
      negotiator.register({
        id: 'data/api',
        name: 'Data API',
        description: 'Data',
        version: '1.0',
        actions: ['read', 'write'],
      });

      expect(negotiator.supportsAction('data/api', 'read')).toBe(true);
      expect(negotiator.supportsAction('data/api', 'delete')).toBe(false);
    });

    test('should support wildcard action', () => {
      const negotiator = new CapabilityNegotiator();
      negotiator.register({
        id: 'admin/api',
        name: 'Admin API',
        description: 'Admin',
        version: '1.0',
        actions: ['*'],
      });

      expect(negotiator.supportsAction('admin/api', 'anything')).toBe(true);
    });

    test('should list all registered capabilities', () => {
      const negotiator = new CapabilityNegotiator();
      negotiator.register({ id: 'cap1', name: 'Cap1', description: '', version: '1.0', actions: [] });
      negotiator.register({ id: 'cap2', name: 'Cap2', description: '', version: '1.0', actions: [] });

      const all = negotiator.getAll();
      expect(all.length).toBe(2);
    });

    test('should get capability by ID', () => {
      const negotiator = new CapabilityNegotiator();
      negotiator.register({
        id: 'test/cap',
        name: 'Test',
        description: 'Test capability',
        version: '2.0',
        actions: ['test'],
      });

      const cap = negotiator.get('test/cap');
      expect(cap?.version).toBe('2.0');
    });
  });

  describe('SequenceTracker', () => {
    test('should start and track sequence', () => {
      const tracker = new SequenceTracker();
      const seq = tracker.start('seq-1', 'weather/forecast', [
        { name: 'request', direction: 'send' },
        { name: 'response', direction: 'receive' },
      ]);

      expect(seq.id).toBe('seq-1');
      expect(seq.currentStep).toBe(0);
      expect(seq.complete).toBe(false);
    });

    test('should get current step', () => {
      const tracker = new SequenceTracker();
      tracker.start('seq-1', 'test', [
        { name: 'step1', direction: 'send' },
        { name: 'step2', direction: 'receive' },
      ]);

      const step = tracker.getCurrentStep('seq-1');
      expect(step?.name).toBe('step1');
    });

    test('should advance through sequence', () => {
      const tracker = new SequenceTracker();
      tracker.start('seq-1', 'test', [
        { name: 'step1', direction: 'send' },
        { name: 'step2', direction: 'receive' },
      ]);

      const result1 = tracker.advance('seq-1');
      expect(result1.success).toBe(true);
      expect(result1.complete).toBe(false);

      const result2 = tracker.advance('seq-1');
      expect(result2.success).toBe(true);
      expect(result2.complete).toBe(true);
    });

    test('should validate step direction', () => {
      const tracker = new SequenceTracker();
      tracker.start('seq-1', 'test', [
        { name: 'step1', direction: 'send' },
      ]);

      const valid = tracker.validateStep('seq-1', 'send', {});
      expect(valid.valid).toBe(true);

      const invalid = tracker.validateStep('seq-1', 'receive', {});
      expect(invalid.valid).toBe(false);
    });

    test('should get active sequences', () => {
      const tracker = new SequenceTracker();
      tracker.start('seq-1', 'test', [{ name: 's1', direction: 'send' }]);
      const seq2 = tracker.start('seq-2', 'test', [{ name: 's1', direction: 'send' }]);
      tracker.complete(seq2.id);

      const active = tracker.getActive();
      expect(active.length).toBe(1);
      expect(active[0]?.id).toBe('seq-1');
    });

    test('should remove sequence', () => {
      const tracker = new SequenceTracker();
      tracker.start('seq-1', 'test', [{ name: 's1', direction: 'send' }]);
      tracker.remove('seq-1');

      expect(tracker.get('seq-1')).toBeUndefined();
    });
  });

  describe('createValidator', () => {
    test('should create validator with builtin schemas', () => {
      const validator = createValidator();

      // Should have builtin schemas loaded
      expect(validator.getRegisteredSchemas().length).toBeGreaterThan(0);
    });
  });

  describe('BUILTIN_SCHEMAS', () => {
    test('should have knock schema', () => {
      expect(BUILTIN_SCHEMAS['agentmesh/knock']).toBeDefined();
    });

    test('should have knock-response schema', () => {
      expect(BUILTIN_SCHEMAS['agentmesh/knock-response']).toBeDefined();
    });

    test('should have message schema', () => {
      expect(BUILTIN_SCHEMAS['agentmesh/message']).toBeDefined();
    });
  });
});
