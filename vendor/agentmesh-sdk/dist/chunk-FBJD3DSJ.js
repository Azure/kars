// src/errors.ts
var AgentMeshError = class extends Error {
  code;
  constructor(message, code) {
    super(message);
    this.name = "AgentMeshError";
    this.code = code;
    Object.setPrototypeOf(this, new.target.prototype);
  }
};
var CryptoError = class extends AgentMeshError {
  constructor(message, code = "CRYPTO_ERROR") {
    super(message, code);
    this.name = "CryptoError";
  }
};
var NetworkError = class extends AgentMeshError {
  statusCode;
  constructor(message, code = "NETWORK_ERROR", statusCode) {
    super(message, code);
    this.name = "NetworkError";
    this.statusCode = statusCode;
  }
};
var SessionError = class extends AgentMeshError {
  constructor(message, code = "SESSION_ERROR") {
    super(message, code);
    this.name = "SessionError";
  }
};
var ValidationError = class extends AgentMeshError {
  details;
  constructor(message, code = "VALIDATION_ERROR", details) {
    super(message, code);
    this.name = "ValidationError";
    this.details = details;
  }
};
var StorageError = class extends AgentMeshError {
  constructor(message, code = "STORAGE_ERROR") {
    super(message, code);
    this.name = "StorageError";
  }
};

export { AgentMeshError, CryptoError, NetworkError, SessionError, StorageError, ValidationError };
//# sourceMappingURL=chunk-FBJD3DSJ.js.map
//# sourceMappingURL=chunk-FBJD3DSJ.js.map