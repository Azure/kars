/**
 * Schemas module for AgentMesh.
 * Handles JSON Schema validation, capability negotiation, and message sequencing.
 */

import { ValidationError } from '../errors';

/**
 * JSON Schema definition (simplified).
 */
export interface JSONSchema {
  type?: string | string[];
  properties?: Record<string, JSONSchema>;
  required?: string[];
  additionalProperties?: boolean;
  items?: JSONSchema;
  minItems?: number;
  maxItems?: number;
  minimum?: number;
  maximum?: number;
  minLength?: number;
  maxLength?: number;
  pattern?: string;
  enum?: unknown[];
  const?: unknown;
  oneOf?: JSONSchema[];
  anyOf?: JSONSchema[];
  allOf?: JSONSchema[];
  $ref?: string;
  $id?: string;
  title?: string;
  description?: string;
}

/**
 * Capability definition.
 */
export interface Capability {
  /** Capability identifier (e.g., 'weather/forecast') */
  id: string;
  /** Human-readable name */
  name: string;
  /** Description */
  description: string;
  /** Version */
  version: string;
  /** Request schema */
  requestSchema?: JSONSchema;
  /** Response schema */
  responseSchema?: JSONSchema;
  /** Supported actions */
  actions: string[];
}

/**
 * Capability negotiation result.
 */
export interface CapabilityMatch {
  /** Matching capability ID */
  capabilityId: string;
  /** Match score (0-1) */
  score: number;
  /** Is exact match? */
  exact: boolean;
  /** Matched via wildcard? */
  wildcard: boolean;
}

/**
 * Message sequence definition.
 */
export interface MessageSequence {
  /** Sequence ID */
  id: string;
  /** Capability this sequence belongs to */
  capabilityId: string;
  /** Expected message order */
  steps: SequenceStep[];
  /** Current step index */
  currentStep: number;
  /** Is sequence complete? */
  complete: boolean;
}

/**
 * Sequence step definition.
 */
export interface SequenceStep {
  /** Step name */
  name: string;
  /** Direction: 'send' or 'receive' */
  direction: 'send' | 'receive';
  /** Schema for this step */
  schema?: JSONSchema;
  /** Is this step optional? */
  optional?: boolean;
}

/**
 * Simple JSON Schema validator.
 */
export class SchemaValidator {
  private schemas: Map<string, JSONSchema> = new Map();

  /**
   * Register a schema.
   */
  register(id: string, schema: JSONSchema): void {
    this.schemas.set(id, schema);
  }

  /**
   * Get a registered schema.
   */
  get(id: string): JSONSchema | undefined {
    return this.schemas.get(id);
  }

  /**
   * Validate data against a schema.
   */
  validate(data: unknown, schema: JSONSchema): { valid: boolean; errors: string[] } {
    const errors: string[] = [];
    this.validateValue(data, schema, '', errors);
    return { valid: errors.length === 0, errors };
  }

  /**
   * Validate data against a registered schema by ID.
   */
  validateById(data: unknown, schemaId: string): { valid: boolean; errors: string[] } {
    const schema = this.schemas.get(schemaId);
    if (!schema) {
      return { valid: false, errors: [`Schema not found: ${schemaId}`] };
    }
    return this.validate(data, schema);
  }

  private validateValue(
    data: unknown,
    schema: JSONSchema,
    path: string,
    errors: string[]
  ): void {
    // Handle $ref
    if (schema.$ref) {
      const refSchema = this.schemas.get(schema.$ref);
      if (refSchema) {
        this.validateValue(data, refSchema, path, errors);
      } else {
        errors.push(`${path}: Unknown schema reference: ${schema.$ref}`);
      }
      return;
    }

    // Handle const
    if (schema.const !== undefined) {
      if (data !== schema.const) {
        errors.push(`${path}: Expected constant value ${JSON.stringify(schema.const)}`);
      }
      return;
    }

    // Handle enum
    if (schema.enum) {
      if (!schema.enum.includes(data)) {
        errors.push(`${path}: Value must be one of: ${schema.enum.map(e => JSON.stringify(e)).join(', ')}`);
      }
      return;
    }

    // Handle oneOf
    if (schema.oneOf) {
      const matches = schema.oneOf.filter(s => {
        const subErrors: string[] = [];
        this.validateValue(data, s, path, subErrors);
        return subErrors.length === 0;
      });
      if (matches.length !== 1) {
        errors.push(`${path}: Must match exactly one of the schemas`);
      }
      return;
    }

    // Handle anyOf
    if (schema.anyOf) {
      const matches = schema.anyOf.some(s => {
        const subErrors: string[] = [];
        this.validateValue(data, s, path, subErrors);
        return subErrors.length === 0;
      });
      if (!matches) {
        errors.push(`${path}: Must match at least one of the schemas`);
      }
      return;
    }

    // Handle allOf
    if (schema.allOf) {
      for (const s of schema.allOf) {
        this.validateValue(data, s, path, errors);
      }
      return;
    }

    // Type validation
    if (schema.type) {
      const types = Array.isArray(schema.type) ? schema.type : [schema.type];
      const actualType = this.getType(data);

      if (!types.includes(actualType)) {
        errors.push(`${path}: Expected type ${types.join(' | ')}, got ${actualType}`);
        return;
      }

      // Type-specific validation
      switch (actualType) {
        case 'object':
          this.validateObject(data as Record<string, unknown>, schema, path, errors);
          break;
        case 'array':
          this.validateArray(data as unknown[], schema, path, errors);
          break;
        case 'string':
          this.validateString(data as string, schema, path, errors);
          break;
        case 'number':
        case 'integer':
          this.validateNumber(data as number, schema, path, errors);
          break;
      }
    }
  }

  private getType(data: unknown): string {
    if (data === null) return 'null';
    if (Array.isArray(data)) return 'array';
    if (typeof data === 'number') {
      return Number.isInteger(data) ? 'integer' : 'number';
    }
    return typeof data;
  }

  private validateObject(
    data: Record<string, unknown>,
    schema: JSONSchema,
    path: string,
    errors: string[]
  ): void {
    // Check required properties
    if (schema.required) {
      for (const prop of schema.required) {
        if (!(prop in data)) {
          errors.push(`${path}: Missing required property: ${prop}`);
        }
      }
    }

    // Validate properties
    if (schema.properties) {
      for (const [key, propSchema] of Object.entries(schema.properties)) {
        if (key in data) {
          this.validateValue(data[key], propSchema, `${path}.${key}`, errors);
        }
      }
    }

    // Check additionalProperties
    if (schema.additionalProperties === false && schema.properties) {
      const allowed = new Set(Object.keys(schema.properties));
      for (const key of Object.keys(data)) {
        if (!allowed.has(key)) {
          errors.push(`${path}: Additional property not allowed: ${key}`);
        }
      }
    }
  }

  private validateArray(
    data: unknown[],
    schema: JSONSchema,
    path: string,
    errors: string[]
  ): void {
    // Check length constraints
    if (schema.minItems !== undefined && data.length < schema.minItems) {
      errors.push(`${path}: Array must have at least ${schema.minItems} items`);
    }
    if (schema.maxItems !== undefined && data.length > schema.maxItems) {
      errors.push(`${path}: Array must have at most ${schema.maxItems} items`);
    }

    // Validate items
    if (schema.items) {
      for (let i = 0; i < data.length; i++) {
        this.validateValue(data[i], schema.items, `${path}[${i}]`, errors);
      }
    }
  }

  private validateString(
    data: string,
    schema: JSONSchema,
    path: string,
    errors: string[]
  ): void {
    if (schema.minLength !== undefined && data.length < schema.minLength) {
      errors.push(`${path}: String must be at least ${schema.minLength} characters`);
    }
    if (schema.maxLength !== undefined && data.length > schema.maxLength) {
      errors.push(`${path}: String must be at most ${schema.maxLength} characters`);
    }
    if (schema.pattern) {
      const regex = new RegExp(schema.pattern);
      if (!regex.test(data)) {
        errors.push(`${path}: String must match pattern: ${schema.pattern}`);
      }
    }
  }

  private validateNumber(
    data: number,
    schema: JSONSchema,
    path: string,
    errors: string[]
  ): void {
    if (schema.minimum !== undefined && data < schema.minimum) {
      errors.push(`${path}: Number must be >= ${schema.minimum}`);
    }
    if (schema.maximum !== undefined && data > schema.maximum) {
      errors.push(`${path}: Number must be <= ${schema.maximum}`);
    }
  }

  /**
   * Get all registered schema IDs.
   */
  getRegisteredSchemas(): string[] {
    return Array.from(this.schemas.keys());
  }

  /**
   * Clear all registered schemas.
   */
  clear(): void {
    this.schemas.clear();
  }
}

/**
 * Capability negotiator for matching capabilities.
 */
export class CapabilityNegotiator {
  private capabilities: Map<string, Capability> = new Map();

  /**
   * Register a capability.
   */
  register(capability: Capability): void {
    this.capabilities.set(capability.id, capability);
  }

  /**
   * Get a capability by ID.
   */
  get(id: string): Capability | undefined {
    return this.capabilities.get(id);
  }

  /**
   * Match a requested capability against registered capabilities.
   * Supports exact match and wildcard patterns.
   */
  match(requested: string): CapabilityMatch[] {
    const matches: CapabilityMatch[] = [];

    // Check for wildcard in request
    const hasWildcard = requested.includes('*');
    const requestPattern = hasWildcard
      ? new RegExp('^' + requested.replace(/\*/g, '.*') + '$')
      : null;

    for (const [id, cap] of this.capabilities) {
      // Exact match
      if (id === requested) {
        matches.push({
          capabilityId: id,
          score: 1.0,
          exact: true,
          wildcard: false,
        });
        continue;
      }

      // Wildcard match (request has wildcard)
      if (requestPattern && requestPattern.test(id)) {
        matches.push({
          capabilityId: id,
          score: 0.8,
          exact: false,
          wildcard: true,
        });
        continue;
      }

      // Prefix match (capability is a prefix of request)
      if (requested.startsWith(id + '/')) {
        matches.push({
          capabilityId: id,
          score: 0.5,
          exact: false,
          wildcard: false,
        });
      }
    }

    // Sort by score descending
    matches.sort((a, b) => b.score - a.score);
    return matches;
  }

  /**
   * Check if a capability supports an action.
   */
  supportsAction(capabilityId: string, action: string): boolean {
    const capability = this.capabilities.get(capabilityId);
    if (!capability) return false;
    return capability.actions.includes(action) || capability.actions.includes('*');
  }

  /**
   * Get all registered capabilities.
   */
  getAll(): Capability[] {
    return Array.from(this.capabilities.values());
  }

  /**
   * Clear all registered capabilities.
   */
  clear(): void {
    this.capabilities.clear();
  }
}

/**
 * Sequence tracker for managing message sequences.
 */
export class SequenceTracker {
  private sequences: Map<string, MessageSequence> = new Map();

  /**
   * Start a new sequence.
   */
  start(id: string, capabilityId: string, steps: SequenceStep[]): MessageSequence {
    const sequence: MessageSequence = {
      id,
      capabilityId,
      steps,
      currentStep: 0,
      complete: false,
    };
    this.sequences.set(id, sequence);
    return sequence;
  }

  /**
   * Get a sequence by ID.
   */
  get(id: string): MessageSequence | undefined {
    return this.sequences.get(id);
  }

  /**
   * Advance the sequence to the next step.
   */
  advance(id: string): { success: boolean; error?: string; complete?: boolean } {
    const sequence = this.sequences.get(id);
    if (!sequence) {
      return { success: false, error: 'Sequence not found' };
    }

    if (sequence.complete) {
      return { success: false, error: 'Sequence already complete' };
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
  getCurrentStep(id: string): SequenceStep | undefined {
    const sequence = this.sequences.get(id);
    if (!sequence || sequence.complete) return undefined;
    return sequence.steps[sequence.currentStep];
  }

  /**
   * Validate that a message matches the expected step.
   */
  validateStep(
    id: string,
    direction: 'send' | 'receive',
    data: unknown,
    validator?: SchemaValidator
  ): { valid: boolean; error?: string } {
    const step = this.getCurrentStep(id);
    if (!step) {
      return { valid: false, error: 'No current step or sequence not found' };
    }

    // Check direction matches
    if (step.direction !== direction) {
      if (step.optional) {
        // Skip optional step
        this.advance(id);
        return this.validateStep(id, direction, data, validator);
      }
      return {
        valid: false,
        error: `Expected ${step.direction} message, got ${direction}`,
      };
    }

    // Validate against schema if provided
    if (step.schema && validator) {
      const result = validator.validate(data, step.schema);
      if (!result.valid) {
        return {
          valid: false,
          error: `Schema validation failed: ${result.errors.join(', ')}`,
        };
      }
    }

    return { valid: true };
  }

  /**
   * Complete the current sequence.
   */
  complete(id: string): void {
    const sequence = this.sequences.get(id);
    if (sequence) {
      sequence.complete = true;
    }
  }

  /**
   * Remove a sequence.
   */
  remove(id: string): void {
    this.sequences.delete(id);
  }

  /**
   * Get all active sequences.
   */
  getActive(): MessageSequence[] {
    return Array.from(this.sequences.values()).filter(s => !s.complete);
  }

  /**
   * Clear all sequences.
   */
  clear(): void {
    this.sequences.clear();
  }
}

/**
 * Built-in AgentMesh protocol schemas.
 */
export const BUILTIN_SCHEMAS: Record<string, JSONSchema> = {
  'agentmesh/knock': {
    type: 'object',
    required: ['version', 'from', 'to', 'request', 'timestamp', 'nonce', 'signature'],
    properties: {
      version: { type: 'string', pattern: '^agentmesh/\\d+\\.\\d+$' },
      from: { type: 'string', minLength: 20 },
      to: { type: 'string', minLength: 20 },
      request: {
        type: 'object',
        required: ['type', 'ttl', 'intent'],
        properties: {
          type: { enum: ['one-shot', 'streaming', 'persistent'] },
          ttl: { type: 'integer', minimum: 1 },
          expectedMessages: { type: 'integer', minimum: 1 },
          intent: {
            type: 'object',
            required: ['capability', 'action'],
            properties: {
              capability: { type: 'string', minLength: 1 },
              action: { type: 'string', minLength: 1 },
              params: { type: 'object' },
            },
          },
        },
      },
      timestamp: { type: 'integer' },
      nonce: { type: 'string', minLength: 16 },
      signature: { type: 'string', minLength: 1 },
      certificateChain: { type: 'array', items: { type: 'string' } },
    },
  },

  'agentmesh/knock-response': {
    type: 'object',
    required: ['type', 'timestamp', 'from', 'to', 'knockNonce', 'signature'],
    properties: {
      type: { enum: ['ACCEPT', 'REJECT'] },
      sessionId: { type: 'string' },
      reason: { type: 'string' },
      timestamp: { type: 'integer' },
      from: { type: 'string', minLength: 20 },
      to: { type: 'string', minLength: 20 },
      knockNonce: { type: 'string', minLength: 16 },
      signature: { type: 'string', minLength: 1 },
    },
  },

  'agentmesh/message': {
    type: 'object',
    required: ['from', 'to', 'sessionId', 'sequence', 'timestamp', 'payload'],
    properties: {
      from: { type: 'string', minLength: 20 },
      to: { type: 'string', minLength: 20 },
      sessionId: { type: 'string', minLength: 1 },
      sequence: { type: 'integer', minimum: 0 },
      timestamp: { type: 'integer' },
      payload: { type: 'object' },
      encrypted: { type: 'boolean' },
    },
  },
};

/**
 * Create a validator with built-in schemas registered.
 */
export function createValidator(): SchemaValidator {
  const validator = new SchemaValidator();
  for (const [id, schema] of Object.entries(BUILTIN_SCHEMAS)) {
    validator.register(id, schema);
  }
  return validator;
}
