// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { AsyncLocalStorage } from "node:async_hooks";
import { request as httpRequest } from "node:http";
import { request as httpsRequest } from "node:https";

/** Auth errors deliberately exclude response bodies, URLs and SDK error messages. */
export async function meshAuthRequest(method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<any> {
  const url = new URL(path);
  if (!["http:", "https:"].includes(url.protocol)) throw new Error("Invalid mesh auth router protocol");
  return new Promise((resolve, reject) => {
    const payload = body === undefined ? undefined : JSON.stringify(body);
    const request = url.protocol === "https:" ? httpsRequest : httpRequest;
    const req = request(url, {
      method, signal,
      headers: payload ? { "content-type": "application/json", "content-length": Buffer.byteLength(payload) } : {},
    }, (res) => {
      let size = 0;
      const chunks: Buffer[] = [];
      res.on("error", () => finish(new Error("Mesh auth response interrupted")));
      res.on("data", (chunk: Buffer) => {
        size += chunk.length;
        if (size > 64 * 1024) {
          finish(new Error("Mesh auth response too large"));
          req.destroy();
          res.destroy();
        } else chunks.push(chunk);
      });
      res.on("end", () => {
        if (!res.statusCode || res.statusCode < 200 || res.statusCode >= 300) {
          finish(new Error("Mesh auth request rejected"));
          return;
        }
        try { finish(undefined, JSON.parse(Buffer.concat(chunks).toString("utf8"))); }
        catch { finish(new Error("Invalid mesh auth response")); }
      });
    });
    let settled = false;
    const timer = setTimeout(() => {
      finish(new Error("Mesh auth request timed out"));
      req.destroy();
    }, 8_000);
    function finish(error?: Error, value?: unknown) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (error) reject(error); else resolve(value);
    }
    req.on("error", () => finish(new Error("Mesh auth request unavailable")));
    req.end(payload);
  });
}

export function validateMeshVerification(result: any, did: string): void {
  if (!did.startsWith("did:mesh:") || result?.did !== did || result?.tier !== "verified"
    || typeof result?.verified_app_id !== "string" || !result.verified_app_id.trim()
    || typeof result?.verified_tenant_id !== "string" || !result.verified_tenant_id.trim()
    || result?.success === false || result?.verified === false || result?.error
    || (result?.status !== undefined && !["verified", "success", "ok"].includes(result.status))) {
    throw new Error("Registry did not verify the expected mesh identity");
  }
}

export interface AuthTransport {
  readonly isConnected: boolean;
  readonly currentDid?: string;
  connect(options?: any): Promise<void>;
  disconnect(): Promise<void>;
  onDisconnect(handler: () => void): void;
  [key: string]: any;
}

export interface AuthHooks {
  publish: () => void;
  revoke: () => void;
  fatal: () => void;
  request: typeof meshAuthRequest;
  verify?: (result: unknown, did: string) => void;
}

type State = "idle" | "authenticating" | "ready" | "retiring" | "failed" | "quarantined" | "closed";

/** One lease and one facade over the existing transport, never another SDK/prekey writer. */
export class RequiredMeshAuth<T extends AuthTransport> {
  readonly client: T;
  state: State = "idle";
  private epoch = 0;
  private expiresAt = 0;
  private token = "";
  private options: any;
  private renewal?: ReturnType<typeof setTimeout>;
  private expiry?: ReturnType<typeof setTimeout>;
  private operation?: Promise<void>;
  private reconnecting?: Promise<void>;
  private retirement?: Promise<void>;
  private failure?: Promise<void>;
  private abort = new AbortController();
  private readonly context = new AsyncLocalStorage<{ epoch: number; admitted: boolean }>();
  private readonly request: typeof meshAuthRequest;

  constructor(private readonly raw: T, private readonly hooks: AuthHooks) {
    this.request = hooks.request;
    // The adapter schedules automatic reconnect before this callback. disconnect()
    // synchronously cancels it; only this lease may reopen and publish the socket.
    raw.onDisconnect(() => {
      if (this.state === "ready") {
        this.revoke();
        void this.reconnect().catch(() => {});
      } else if (this.state === "authenticating") {
        void this.fail();
      }
    });
    this.client = new Proxy(raw, {
      get: (target, key) => {
        if (key === "isConnected") return this.usable && target.isConnected;
        if (key === "connect") return (options?: any) => this.connect(options);
        if (key === "disconnect") return () => {
          const context = this.context.getStore();
          if (context && (!context.admitted || context.epoch !== this.epoch)) return Promise.reject(new Error("Stale mesh operation"));
          return this.close();
        };
        const value: any = Reflect.get(target, key, target);
        if (typeof value !== "function") return value;
        if (typeof key === "string" && ["onMessage", "onKnock", "onError", "onE2EVerified"].includes(key)) {
          return (handler: (...args: any[]) => any) => value.call(target, (...args: any[]) => {
            if (!this.usable) return key === "onKnock" ? { accept: false, reason: "Required Entra authentication unavailable" } : undefined;
            const epoch = this.epoch;
            const result = this.context.run({ epoch, admitted: true }, () => handler(...args));
            if (key === "onKnock") {
              return Promise.resolve(result).then((allowed) => this.usable && epoch === this.epoch
                ? allowed : { accept: false, reason: "Required Entra authentication unavailable" });
            }
            return result;
          });
        }
        if (key === "onDisconnect" || key === "enableKnockEnforcement") return value.bind(target);
        return (...args: any[]) => {
          this.assertUsable();
          const epoch = this.epoch;
          // Bind composite helpers (sendFile/sendWithAck) through the same facade
          // so their delayed internal sends cannot bypass the lease.
          const result = this.context.run({ epoch, admitted: true }, () => value.apply(this.client, args));
          if (result && typeof result.then === "function") {
            return Promise.resolve(result).then((value) => {
              this.assertUsable();
              if (epoch !== this.epoch) throw new Error("Stale mesh operation completion");
              return value;
            });
          }
          return result;
        };
      },
    });
  }

  get usable(): boolean {
    const context = this.context.getStore();
    return this.state === "ready" && this.raw.isConnected && Date.now() < this.expiresAt
      && (!context || (context.admitted && context.epoch === this.epoch));
  }

  assertUsable(): void {
    if (!this.usable) throw new Error("Required Entra mesh authentication unavailable");
  }

  /** Carry socket custody through asynchronous SDK decryption and callbacks. */
  bindSocketMessages<TSocket extends { emit: (event: string | symbol, ...args: any[]) => boolean }>(socket: TSocket): TSocket {
    const epoch = this.epoch;
    const emit = socket.emit.bind(socket);
    socket.emit = (event, ...args) => {
      if (event !== "message") return emit(event, ...args);
      if (epoch !== this.epoch || !["authenticating", "ready"].includes(this.state)) return false;
      return this.context.run({ epoch, admitted: this.state === "ready" }, () => emit(event, ...args));
    };
    return socket;
  }

  /** Called for every connect frame, including SDK retries, not captured from env. */
  connectToken(): string {
    if (!["authenticating", "ready"].includes(this.state) || Date.now() >= this.expiresAt || !this.token) {
      throw new Error("Required Entra connect token unavailable");
    }
    return this.token;
  }

  connect(options?: any): Promise<void> {
    const context = this.context.getStore();
    if (context && (!context.admitted || context.epoch !== this.epoch)) return Promise.reject(new Error("Stale mesh operation"));
    if (["failed", "quarantined", "closed"].includes(this.state)) {
      return Promise.reject(new Error("Required Entra mesh stopped; process restart required"));
    }
    this.options = options ?? this.options;
    if (this.state === "retiring") return this.reconnect();
    if (this.operation) return this.operation;
    if (this.usable) return Promise.resolve();
    if (this.state === "ready") {
      this.revoke();
      return this.reconnect();
    }
    return this.startAuthentication();
  }

  private startAuthentication(): Promise<void> {
    this.operation = this.context.exit(() => this.authenticate()).finally(() => { this.operation = undefined; });
    return this.operation;
  }

  private async authenticate(): Promise<void> {
    const epoch = ++this.epoch;
    this.state = "authenticating";
    const started = Date.now();
    try {
      const minted = await this.request("GET", "/v1/mesh-token", undefined, this.abort.signal);
      this.checkEpoch(epoch);
      if (typeof minted?.access_token !== "string" || !minted.access_token || /\s/.test(minted.access_token)
        || minted.access_token.length > 8192 || typeof minted.token_type !== "string" || minted.token_type.toLowerCase() !== "bearer"
        || typeof minted.expires_in !== "number" || !Number.isFinite(minted.expires_in)
        || minted.expires_in <= 60 || minted.expires_in > 86400 || minted.error || minted.success === false) {
        throw new Error("Invalid mesh token lease");
      }
      this.expiresAt = started + minted.expires_in * 1000;
      // An exp claim can only shorten the router's lease; verification remains the registry's job.
      try {
        const claims = JSON.parse(Buffer.from(minted.access_token.split(".")[1], "base64url").toString());
        if (typeof claims.exp === "number") this.expiresAt = Math.min(this.expiresAt, claims.exp * 1000);
      } catch { /* Opaque tokens use the router's expires_in contract. */ }
      if (this.expiresAt - Date.now() <= 60_000) throw new Error("Mesh token lease too short");
      this.token = minted.access_token;
      this.expiry = setTimeout(() => { void this.fail(); }, this.expiresAt - Date.now());
      this.expiry.unref?.();
      await deadline(this.raw.connect(this.options), 35_000);
      this.checkEpoch(epoch);
      const did = this.raw.currentDid;
      if (!this.raw.isConnected || typeof did !== "string" || !/^did:mesh:[a-f0-9]{32}$/.test(did)) {
        throw new Error("Mesh transport did not register a canonical identity");
      }
      const verified = await this.request("POST", "/agt/registry/v1/registry/verify", {
        amid: did, verification_token: this.token,
      }, this.abort.signal);
      this.checkEpoch(epoch);
      validateMeshVerification(verified, did);
      this.hooks.verify?.(verified, did);
      if (!this.raw.isConnected || this.raw.currentDid !== did || Date.now() >= this.expiresAt) {
        throw new Error("Mesh lease or identity changed before verification");
      }
      // The transport inbox also receives pre-verification callbacks; never expose
      // those queued messages after admission, even though listeners rejected them.
      this.raw.drainInbox?.();
      this.state = "ready";
      const remaining = this.expiresAt - Date.now();
      this.renewal = setTimeout(() => {
        this.revoke();
        void this.reconnect().catch(() => {});
      }, remaining - Math.min(300_000, remaining / 2));
      this.renewal.unref?.();
      this.hooks.publish();
    } catch {
      await this.fail();
      throw new Error("Required Entra mesh authentication failed; process restart required");
    }
  }

  private checkEpoch(epoch: number): void {
    if (epoch !== this.epoch || this.state !== "authenticating") throw new Error("Stale mesh authentication completion");
  }

  private revoke(): void {
    ++this.epoch;
    clearTimeout(this.renewal);
    clearTimeout(this.expiry);
    this.token = "";
    this.state = "retiring";
    this.hooks.revoke();
  }

  private retire(): Promise<void> {
    if (this.retirement) return this.retirement;
    // Invoke before awaiting anything: this cancels the adapter's reconnect timer.
    try { this.retirement = deadline(this.raw.disconnect(), 10_000); }
    catch { this.retirement = Promise.reject(new Error("Mesh retirement failed")); }
    return this.retirement;
  }

  private reconnect(): Promise<void> {
    if (this.reconnecting) return this.reconnecting;
    const previous = this.operation;
    const retirement = this.retire();
    this.reconnecting = (async () => {
      await retirement;
      await previous;
      if (this.state !== "retiring") return;
      this.retirement = undefined;
      await this.startAuthentication();
    })().catch(async () => {
      await this.fail();
      throw new Error("Required Entra mesh reconnect failed; process restart required");
    }).finally(() => { this.reconnecting = undefined; });
    return this.reconnecting;
  }

  /** Terminal: retain process/prekey custody even if retirement fails. */
  fail(): Promise<void> {
    if (this.failure) return this.failure;
    this.revoke();
    this.state = "failed";
    this.abort.abort();
    this.failure = (async () => {
      try { await this.retire(); }
      catch { this.state = "quarantined"; }
      this.hooks.fatal();
    })();
    return this.failure;
  }

  async close(): Promise<void> {
    if (["failed", "quarantined"].includes(this.state)) { await this.failure; return; }
    this.revoke();
    this.state = "closed";
    this.abort.abort();
    try { await this.retire(); }
    catch { this.state = "quarantined"; this.hooks.fatal(); }
  }
}

async function deadline<T>(operation: Promise<T>, timeout: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([operation, new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error("Mesh lifecycle operation timed out")), timeout);
    })]);
  } finally { clearTimeout(timer); }
}
