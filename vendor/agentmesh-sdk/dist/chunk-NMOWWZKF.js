import { NetworkError } from './chunk-FBJD3DSJ.js';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { EventEmitter } from 'events';

// src/config/policy.ts
var Tier = /* @__PURE__ */ ((Tier2) => {
  Tier2["ANONYMOUS"] = "anonymous";
  Tier2["VERIFIED"] = "verified";
  Tier2["ORGANIZATION"] = "organization";
  return Tier2;
})(Tier || {});
var TierLevel = {
  ["anonymous" /* ANONYMOUS */]: 0,
  ["verified" /* VERIFIED */]: 1,
  ["organization" /* ORGANIZATION */]: 2
};
function getTierLevel(tier) {
  return TierLevel[tier] ?? 0;
}
var Policy = class _Policy {
  minTier;
  minReputation;
  allowedIntents;
  blockedIntents;
  allowedAmids;
  blockedAmids;
  autoAccept;
  maxSessionTtl;
  maxConcurrentSessions;
  constructor(options = {}) {
    this.minTier = options.minTier ?? "anonymous" /* ANONYMOUS */;
    this.minReputation = options.minReputation ?? 0;
    this.allowedIntents = new Set(options.allowedIntents ?? []);
    this.blockedIntents = new Set(options.blockedIntents ?? []);
    this.allowedAmids = new Set(options.allowedAmids ?? []);
    this.blockedAmids = new Set(options.blockedAmids ?? []);
    this.autoAccept = options.autoAccept ?? false;
    this.maxSessionTtl = options.maxSessionTtl ?? 300;
    this.maxConcurrentSessions = options.maxConcurrentSessions ?? 100;
  }
  /**
   * Evaluate whether a KNOCK should be accepted based on this policy.
   */
  evaluate(knock) {
    if (this.blockedAmids.has(knock.fromAmid)) {
      return { allowed: false, reason: "AMID is blocked" };
    }
    if (this.allowedAmids.size > 0 && !this.allowedAmids.has(knock.fromAmid)) {
      return { allowed: false, reason: "AMID not in allow list" };
    }
    if (getTierLevel(knock.fromTier) < getTierLevel(this.minTier)) {
      return { allowed: false, reason: `Tier ${knock.fromTier} below minimum ${this.minTier}` };
    }
    if (knock.fromReputation < this.minReputation) {
      return {
        allowed: false,
        reason: `Reputation ${knock.fromReputation} below minimum ${this.minReputation}`
      };
    }
    const intentCategory = knock.intentCategory;
    if (intentCategory && this.blockedIntents.has(intentCategory)) {
      return { allowed: false, reason: `Intent category ${intentCategory} is blocked` };
    }
    if (this.allowedIntents.size > 0 && intentCategory && !this.allowedIntents.has(intentCategory)) {
      return { allowed: false, reason: `Intent category ${intentCategory} not in allow list` };
    }
    if (knock.requestedTtl > this.maxSessionTtl) {
      return {
        allowed: false,
        reason: `Requested TTL ${knock.requestedTtl}s exceeds maximum ${this.maxSessionTtl}s`
      };
    }
    return {
      allowed: true,
      autoAccept: this.autoAccept
    };
  }
  /**
   * Create a permissive policy that accepts all KNOCKs.
   */
  static permissive() {
    return new _Policy({ autoAccept: true });
  }
  /**
   * Create a restrictive policy that only accepts verified agents.
   */
  static verified() {
    return new _Policy({
      minTier: "verified" /* VERIFIED */,
      minReputation: 0.5
    });
  }
  /**
   * Create a policy that only accepts organization-verified agents.
   */
  static organization() {
    return new _Policy({
      minTier: "organization" /* ORGANIZATION */,
      minReputation: 0.7
    });
  }
};
var Config = class _Config {
  registryUrl;
  relayUrl;
  policy;
  displayName;
  capabilities;
  enableP2P;
  connectionTimeout;
  maxReconnectAttempts;
  debug;
  constructor(options = {}) {
    this.registryUrl = options.registryUrl ?? "https://agentmesh.online/v1";
    this.relayUrl = options.relayUrl ?? "wss://relay.agentmesh.online/v1/connect";
    this.policy = options.policy ?? new Policy();
    this.displayName = options.displayName;
    this.capabilities = options.capabilities ?? [];
    this.enableP2P = options.enableP2P ?? false;
    this.connectionTimeout = options.connectionTimeout ?? 3e4;
    this.maxReconnectAttempts = options.maxReconnectAttempts ?? 5;
    this.debug = options.debug ?? false;
  }
  /**
   * Create configuration with defaults.
   */
  static default() {
    return new _Config();
  }
  /**
   * Create configuration for development.
   */
  static development(options = {}) {
    return new _Config({
      debug: true,
      policy: Policy.permissive(),
      ...options
    });
  }
};
var ConfigError = class extends Error {
  constructor(message, cause) {
    super(message);
    this.cause = cause;
    this.name = "ConfigError";
  }
};
var FileConfigLoader = class extends EventEmitter {
  baseDir;
  keysDir;
  sessionsDir;
  policyPath;
  useFileStorage;
  gracefulFallback;
  policyWatcher = null;
  currentPolicy = null;
  constructor(options = {}) {
    super();
    this.baseDir = options.baseDir || this.resolveBaseDir();
    this.keysDir = path.join(this.baseDir, "keys");
    this.sessionsDir = path.join(this.baseDir, "sessions");
    this.policyPath = path.join(this.baseDir, "policy.json");
    this.useFileStorage = options.useFileStorage ?? true;
    this.gracefulFallback = options.gracefulFallback ?? true;
    if (this.useFileStorage) {
      this.ensureDirectories();
    }
    if (options.watchPolicy && this.useFileStorage) {
      this.startPolicyWatcher();
    }
  }
  /**
   * Resolve the base directory.
   */
  resolveBaseDir() {
    if (process.env.AGENTMESH_HOME) {
      return process.env.AGENTMESH_HOME;
    }
    return path.join(os.homedir(), ".agentmesh");
  }
  /**
   * Ensure required directories exist with proper permissions.
   */
  ensureDirectories() {
    try {
      const dirs = [this.baseDir, this.keysDir, this.sessionsDir];
      for (const dir of dirs) {
        if (!fs.existsSync(dir)) {
          fs.mkdirSync(dir, { recursive: true, mode: 448 });
        }
      }
    } catch (error) {
      if (this.gracefulFallback) {
        console.warn(`Failed to create AgentMesh directories: ${error}`);
      } else {
        throw new ConfigError("Failed to create AgentMesh directories", error);
      }
    }
  }
  /**
   * Get the base directory path.
   */
  getBaseDir() {
    return this.baseDir;
  }
  /**
   * Get the keys directory path.
   */
  getKeysDir() {
    return this.keysDir;
  }
  /**
   * Get the sessions directory path.
   */
  getSessionsDir() {
    return this.sessionsDir;
  }
  /**
   * Load policy from file.
   */
  loadPolicy(policyPath) {
    const filePath = policyPath || this.policyPath;
    try {
      if (!fs.existsSync(filePath)) {
        this.currentPolicy = new Policy({});
        return this.currentPolicy;
      }
      const content = fs.readFileSync(filePath, "utf-8");
      const policyData = JSON.parse(content);
      this.validatePolicyContent(policyData);
      this.currentPolicy = new Policy(policyData);
      return this.currentPolicy;
    } catch (error) {
      if (this.gracefulFallback) {
        console.warn(`Failed to load policy, using defaults: ${error}`);
        this.currentPolicy = new Policy({});
        return this.currentPolicy;
      }
      if (error instanceof SyntaxError) {
        throw new ConfigError(`Invalid JSON in policy file: ${filePath}`, error);
      }
      if (error instanceof ConfigError) {
        throw error;
      }
      throw new ConfigError(`Failed to load policy from ${filePath}`, error);
    }
  }
  /**
   * Validate policy content.
   */
  validatePolicyContent(data) {
    if (typeof data !== "object" || data === null) {
      throw new ConfigError("Policy must be an object");
    }
    const policy = data;
    if (policy.minTier !== void 0) {
      const validTiers = ["anonymous", "verified", "organization"];
      if (typeof policy.minTier !== "string" || !validTiers.includes(policy.minTier)) {
        throw new ConfigError('minTier must be "anonymous", "verified", or "organization"');
      }
    }
    if (policy.minReputation !== void 0) {
      if (typeof policy.minReputation !== "number" || policy.minReputation < 0 || policy.minReputation > 1) {
        throw new ConfigError("minReputation must be a number between 0 and 1");
      }
    }
    if (policy.maxConcurrentSessions !== void 0) {
      if (typeof policy.maxConcurrentSessions !== "number" || policy.maxConcurrentSessions < 1) {
        throw new ConfigError("maxConcurrentSessions must be a positive number");
      }
    }
    if (policy.blockedAmids !== void 0) {
      if (!Array.isArray(policy.blockedAmids)) {
        throw new ConfigError("blockedAmids must be an array");
      }
    }
    if (policy.allowedAmids !== void 0) {
      if (!Array.isArray(policy.allowedAmids)) {
        throw new ConfigError("allowedAmids must be an array");
      }
    }
    if (policy.acceptedIntents !== void 0) {
      if (!Array.isArray(policy.acceptedIntents)) {
        throw new ConfigError("acceptedIntents must be an array");
      }
    }
  }
  /**
   * Save policy to file.
   */
  savePolicy(policy, policyPath) {
    const filePath = policyPath || this.policyPath;
    try {
      const content = JSON.stringify(policy, null, 2);
      fs.writeFileSync(filePath, content, { mode: 384 });
    } catch (error) {
      throw new ConfigError(`Failed to save policy to ${filePath}`, error);
    }
  }
  /**
   * Start watching policy file for changes.
   */
  startPolicyWatcher() {
    if (this.policyWatcher) {
      return;
    }
    try {
      if (!fs.existsSync(this.policyPath)) {
        this.savePolicy({});
      }
      this.policyWatcher = fs.watch(this.policyPath, (eventType) => {
        if (eventType === "change") {
          this.reloadPolicy();
        }
      });
    } catch (error) {
      console.warn(`Failed to watch policy file: ${error}`);
    }
  }
  /**
   * Reload policy from file.
   */
  reloadPolicy() {
    try {
      const newPolicy = this.loadPolicy();
      this.currentPolicy = newPolicy;
      this.emit("policy_reloaded", newPolicy);
    } catch (error) {
      this.emit("policy_reload_failed", error);
    }
  }
  /**
   * Get the current policy.
   */
  getPolicy() {
    return this.currentPolicy;
  }
  /**
   * Persist a session to disk.
   */
  persistSession(session) {
    if (!this.useFileStorage) {
      return;
    }
    try {
      const filePath = path.join(this.sessionsDir, `${session.sessionId}.json`);
      const content = JSON.stringify(session, null, 2);
      fs.writeFileSync(filePath, content, { mode: 384 });
      this.emit("session_persisted", session.sessionId);
    } catch (error) {
      if (!this.gracefulFallback) {
        throw new ConfigError(`Failed to persist session ${session.sessionId}`, error);
      }
    }
  }
  /**
   * Restore all sessions from disk.
   */
  restoreSessions() {
    if (!this.useFileStorage) {
      return [];
    }
    const sessions = [];
    try {
      if (!fs.existsSync(this.sessionsDir)) {
        return [];
      }
      const files = fs.readdirSync(this.sessionsDir);
      const now = /* @__PURE__ */ new Date();
      for (const file of files) {
        if (!file.endsWith(".json")) {
          continue;
        }
        try {
          const filePath = path.join(this.sessionsDir, file);
          const content = fs.readFileSync(filePath, "utf-8");
          const session = JSON.parse(content);
          const expiresAt = new Date(session.expiresAt);
          if (expiresAt <= now) {
            fs.unlinkSync(filePath);
            continue;
          }
          sessions.push(session);
          this.emit("session_restored", session.sessionId);
        } catch {
        }
      }
    } catch (error) {
      if (!this.gracefulFallback) {
        throw new ConfigError("Failed to restore sessions", error);
      }
    }
    return sessions;
  }
  /**
   * Delete a persisted session.
   */
  deleteSession(sessionId) {
    if (!this.useFileStorage) {
      return;
    }
    try {
      const filePath = path.join(this.sessionsDir, `${sessionId}.json`);
      if (fs.existsSync(filePath)) {
        fs.unlinkSync(filePath);
      }
    } catch {
    }
  }
  /**
   * Clean up expired sessions.
   */
  cleanupExpiredSessions() {
    if (!this.useFileStorage) {
      return 0;
    }
    let cleaned = 0;
    try {
      if (!fs.existsSync(this.sessionsDir)) {
        return 0;
      }
      const files = fs.readdirSync(this.sessionsDir);
      const now = /* @__PURE__ */ new Date();
      for (const file of files) {
        if (!file.endsWith(".json")) {
          continue;
        }
        try {
          const filePath = path.join(this.sessionsDir, file);
          const content = fs.readFileSync(filePath, "utf-8");
          const session = JSON.parse(content);
          const expiresAt = new Date(session.expiresAt);
          if (expiresAt <= now) {
            fs.unlinkSync(filePath);
            cleaned++;
          }
        } catch {
        }
      }
      if (cleaned > 0) {
        this.emit("session_cleanup", cleaned);
      }
    } catch (error) {
      if (!this.gracefulFallback) {
        throw new ConfigError("Failed to cleanup sessions", error);
      }
    }
    return cleaned;
  }
  /**
   * Stop watching policy file.
   */
  stopWatching() {
    if (this.policyWatcher) {
      this.policyWatcher.close();
      this.policyWatcher = null;
    }
  }
  /**
   * Close the file config loader.
   */
  close() {
    this.stopWatching();
    this.removeAllListeners();
  }
};
function createFileConfigLoader(options) {
  return new FileConfigLoader(options);
}

// src/discovery.ts
var RegistryClient = class {
  baseUrl;
  timeout;
  constructor(baseUrl = "https://agentmesh.online/v1", timeout = 3e4) {
    this.baseUrl = baseUrl.replace(/\/$/, "");
    this.timeout = timeout;
  }
  /**
   * Register this agent with the registry.
   */
  async register(identity, options = {}) {
    const [timestamp, signature] = await identity.signTimestamp();
    const payload = {
      amid: identity.amid,
      signing_public_key: identity.signingPublicKeyB64,
      exchange_public_key: identity.exchangePublicKeyB64,
      display_name: options.displayName,
      capabilities: options.capabilities ?? [],
      relay_endpoint: options.relayEndpoint ?? "wss://relay.agentmesh.online/v1/connect",
      direct_endpoint: options.directEndpoint,
      verification_token: options.verificationToken,
      timestamp,
      signature
    };
    try {
      const response = await this.fetch("/registry/register", {
        method: "POST",
        body: JSON.stringify(payload)
      });
      const data = await response.json();
      if (response.status === 201) {
        return { success: true, ...data };
      } else if (response.status === 409) {
        return { success: true, alreadyRegistered: true, ...data };
      } else {
        return { success: false, error: data.error ?? "Unknown error" };
      }
    } catch (error) {
      throw new NetworkError(
        `Registry connection error: ${error instanceof Error ? error.message : "Unknown error"}`,
        "REGISTRY_ERROR"
      );
    }
  }
  /**
   * Look up an agent by AMID.
   */
  async lookup(amid) {
    try {
      const response = await this.fetch(`/registry/lookup?amid=${encodeURIComponent(amid)}`);
      if (response.status === 200) {
        const data = await response.json();
        return this.parseAgentInfo(data);
      } else if (response.status === 404) {
        return null;
      } else {
        throw new NetworkError(`Lookup failed: ${response.status}`, "LOOKUP_ERROR", response.status);
      }
    } catch (error) {
      if (error instanceof NetworkError) throw error;
      throw new NetworkError(
        `Registry connection error: ${error instanceof Error ? error.message : "Unknown error"}`,
        "REGISTRY_ERROR"
      );
    }
  }
  /**
   * Search for agents by capability.
   */
  async search(options) {
    const params = new URLSearchParams();
    params.set("capability", options.capability);
    if (options.tierMin !== void 0) params.set("tier_min", String(options.tierMin));
    if (options.reputationMin !== void 0) params.set("reputation_min", String(options.reputationMin));
    if (options.status) params.set("status", options.status);
    params.set("limit", String(options.limit ?? 20));
    params.set("offset", String(options.offset ?? 0));
    try {
      const response = await this.fetch(`/registry/search?${params}`);
      if (response.status === 200) {
        const data = await response.json();
        return {
          results: data.results.map((r) => this.parseAgentInfo(r)),
          total: data.total
        };
      } else {
        throw new NetworkError(`Search failed: ${response.status}`, "SEARCH_ERROR", response.status);
      }
    } catch (error) {
      if (error instanceof NetworkError) throw error;
      throw new NetworkError(
        `Registry connection error: ${error instanceof Error ? error.message : "Unknown error"}`,
        "REGISTRY_ERROR"
      );
    }
  }
  /**
   * Update agent presence status.
   */
  async updateStatus(identity, status) {
    const [timestamp, signature] = await identity.signTimestamp();
    const payload = {
      amid: identity.amid,
      status,
      timestamp,
      signature
    };
    try {
      const response = await this.fetch("/registry/status", {
        method: "POST",
        body: JSON.stringify(payload)
      });
      return response.status === 200;
    } catch {
      return false;
    }
  }
  /**
   * Update agent capabilities.
   */
  async updateCapabilities(identity, capabilities) {
    const [timestamp, signature] = await identity.signTimestamp();
    const payload = {
      amid: identity.amid,
      capabilities,
      timestamp,
      signature
    };
    try {
      const response = await this.fetch("/registry/capabilities", {
        method: "POST",
        body: JSON.stringify(payload)
      });
      return response.status === 200;
    } catch {
      return false;
    }
  }
  /**
   * Upload X3DH prekeys to registry.
   */
  async uploadPrekeys(identity, signedPrekey, signedPrekeySignature, signedPrekeyId, oneTimePrekeys) {
    const [timestamp, signature] = await identity.signTimestamp();
    const payload = {
      amid: identity.amid,
      signed_prekey: signedPrekey,
      signed_prekey_signature: signedPrekeySignature,
      signed_prekey_id: signedPrekeyId,
      one_time_prekeys: oneTimePrekeys,
      timestamp,
      signature
    };
    try {
      const response = await this.fetch("/registry/prekeys", {
        method: "POST",
        body: JSON.stringify(payload)
      });
      return response.status === 200;
    } catch {
      return false;
    }
  }
  /**
   * Fetch prekeys for an agent (for X3DH key exchange).
   */
  async getPrekeys(amid) {
    try {
      const response = await this.fetch(`/registry/prekeys/${encodeURIComponent(amid)}`);
      if (response.status === 200) {
        const data = await response.json();
        return {
          identityKey: data.identity_key,
          signedPrekey: data.signed_prekey,
          signedPrekeySignature: data.signed_prekey_signature,
          signedPrekeyId: data.signed_prekey_id,
          oneTimePrekeys: data.one_time_prekeys ?? []
        };
      } else if (response.status === 404) {
        return null;
      } else {
        throw new NetworkError(`Prekey fetch failed: ${response.status}`, "PREKEY_ERROR", response.status);
      }
    } catch (error) {
      if (error instanceof NetworkError) throw error;
      return null;
    }
  }
  /**
   * Get available OAuth providers for tier verification.
   */
  async getOAuthProviders() {
    try {
      const response = await this.fetch("/auth/oauth/providers");
      if (response.status === 200) {
        const data = await response.json();
        return data.providers ?? [];
      } else {
        return [];
      }
    } catch {
      return [];
    }
  }
  /**
   * Start OAuth verification flow for tier upgrade.
   */
  async startOAuthVerification(identity, provider) {
    const [timestamp, signature] = await identity.signTimestamp();
    const payload = {
      amid: identity.amid,
      provider,
      timestamp,
      signature
    };
    try {
      const response = await this.fetch("/auth/oauth/authorize", {
        method: "POST",
        body: JSON.stringify(payload)
      });
      if (response.status === 200) {
        const data = await response.json();
        return {
          authorizationUrl: data.authorization_url,
          state: data.state
        };
      } else {
        return null;
      }
    } catch {
      return null;
    }
  }
  /**
   * Get verification status for an agent.
   */
  async getVerificationStatus(amid) {
    const info = await this.lookup(amid);
    if (!info) return null;
    return {
      tier: info.tier,
      isVerified: info.tier === "verified" /* VERIFIED */ || info.tier === "organization" /* ORGANIZATION */
    };
  }
  /**
   * Check if an agent's certificate has been revoked.
   */
  async checkRevocation(amid) {
    try {
      const response = await this.fetch(`/registry/revocation?amid=${encodeURIComponent(amid)}`);
      if (response.status === 200) {
        return await response.json();
      } else {
        return { revoked: false };
      }
    } catch {
      return { revoked: false };
    }
  }
  /**
   * Check revocation status for multiple agents at once.
   */
  async bulkCheckRevocation(amids) {
    try {
      const response = await this.fetch("/registry/revocations/bulk", {
        method: "POST",
        body: JSON.stringify({ amids })
      });
      if (response.status === 200) {
        const data = await response.json();
        return data.revocations ?? {};
      } else {
        return {};
      }
    } catch {
      return {};
    }
  }
  /**
   * Check registry health.
   */
  async healthCheck() {
    try {
      const response = await this.fetch("/health");
      if (response.status === 200) {
        return await response.json();
      } else {
        return { status: "unhealthy" };
      }
    } catch {
      return { status: "unreachable" };
    }
  }
  /**
   * Submit reputation feedback for another agent.
   */
  async submitReputation(identity, targetAmid, sessionId, score, tags) {
    if (score < 0 || score > 1) {
      throw new Error("Score must be between 0.0 and 1.0");
    }
    const [timestamp, signature] = await identity.signTimestamp();
    const payload = {
      target_amid: targetAmid,
      from_amid: identity.amid,
      session_id: sessionId,
      score,
      tags,
      timestamp,
      signature
    };
    try {
      const response = await this.fetch("/registry/reputation", {
        method: "POST",
        body: JSON.stringify(payload)
      });
      if (response.status !== 200) {
        const errText = await response.text().catch(() => "");
        console.error(`[agentmesh-sdk] submitReputation rejected: ${response.status} ${errText} (from=${identity.amid} target=${targetAmid})`);
      }
      return response.status === 200;
    } catch (err) {
      console.error(`[agentmesh-sdk] submitReputation error: ${err?.message || err} (from=${identity.amid} target=${targetAmid})`);
      return false;
    }
  }
  /**
   * Internal fetch helper with timeout.
   */
  async fetch(path2, options = {}) {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), this.timeout);
    try {
      const response = await fetch(`${this.baseUrl}${path2}`, {
        ...options,
        headers: {
          "Content-Type": "application/json",
          ...options.headers
        },
        signal: controller.signal
      });
      return response;
    } finally {
      clearTimeout(timeoutId);
    }
  }
  /**
   * Parse agent info from API response.
   * Handles both camelCase and snake_case field names for compatibility.
   */
  parseAgentInfo(data) {
    const get = (camel, snake) => data[camel] ?? data[snake];
    const lastSeenStr = get("lastSeen", "last_seen") ?? (/* @__PURE__ */ new Date()).toISOString();
    const lastSeen = new Date(lastSeenStr.endsWith("Z") ? lastSeenStr : lastSeenStr.replace("Z", "+00:00"));
    return {
      amid: data.amid,
      tier: data.tier,
      displayName: get("displayName", "display_name"),
      organization: data.organization,
      signingPublicKey: get("signingPublicKey", "signing_public_key") ?? "",
      exchangePublicKey: get("exchangePublicKey", "exchange_public_key") ?? "",
      capabilities: data.capabilities ?? [],
      relayEndpoint: get("relayEndpoint", "relay_endpoint") ?? "",
      directEndpoint: get("directEndpoint", "direct_endpoint"),
      status: data.status,
      reputationScore: get("reputationScore", "reputation_score") ?? 0.5,
      lastSeen
    };
  }
};

export { Config, ConfigError, FileConfigLoader, Policy, RegistryClient, Tier, TierLevel, createFileConfigLoader, getTierLevel };
//# sourceMappingURL=chunk-NMOWWZKF.js.map
//# sourceMappingURL=chunk-NMOWWZKF.js.map