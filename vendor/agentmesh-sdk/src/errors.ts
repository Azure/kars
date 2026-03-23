/**
 * Base error class for AgentMesh SDK errors.
 */
export class AgentMeshError extends Error {
  readonly code: string;

  constructor(message: string, code: string) {
    super(message);
    this.name = 'AgentMeshError';
    this.code = code;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/**
 * Error thrown for cryptographic operation failures.
 */
export class CryptoError extends AgentMeshError {
  constructor(message: string, code = 'CRYPTO_ERROR') {
    super(message, code);
    this.name = 'CryptoError';
  }
}

/**
 * Error thrown for network-related failures.
 */
export class NetworkError extends AgentMeshError {
  readonly statusCode?: number;

  constructor(message: string, code = 'NETWORK_ERROR', statusCode?: number) {
    super(message, code);
    this.name = 'NetworkError';
    this.statusCode = statusCode;
  }
}

/**
 * Error thrown for session-related failures.
 */
export class SessionError extends AgentMeshError {
  constructor(message: string, code = 'SESSION_ERROR') {
    super(message, code);
    this.name = 'SessionError';
  }
}

/**
 * Error thrown for validation failures.
 */
export class ValidationError extends AgentMeshError {
  readonly details?: Record<string, unknown>;

  constructor(message: string, code = 'VALIDATION_ERROR', details?: Record<string, unknown>) {
    super(message, code);
    this.name = 'ValidationError';
    this.details = details;
  }
}

/**
 * Error thrown for storage-related failures.
 */
export class StorageError extends AgentMeshError {
  constructor(message: string, code = 'STORAGE_ERROR') {
    super(message, code);
    this.name = 'StorageError';
  }
}
