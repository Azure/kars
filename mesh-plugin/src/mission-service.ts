// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomUUID } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { setTimeout as delay } from "node:timers/promises";
import { dispatcherCustody, type DispatcherCustody, type DispatcherLocation } from "./mission-bootstrap.js";
import { MissionDispatcher, type MissionCandidate } from "./mission-dispatcher.js";
import { acquireMissionWriter, missionIdentity } from "./mission-identity.js";
import { KubernetesMissionStore } from "./mission-store.js";
import type { KubernetesJson } from "./kubernetes-json.js";
import { createMeshTransport } from "./transport-factory.js";
import type { IMeshTransport } from "./transport-interface.js";

export interface MissionServiceConfig {
  location: DispatcherLocation;
  root: string;
  relayUrl: string;
  registryUrl: string;
  pollMs: number;
  timeoutMs: number;
}
export type MissionServiceEvent = "custody-unavailable" | "discovery-unavailable" | "candidate-unavailable" | "transport-error";
export interface MissionServiceDependencies {
  acquireWriter: typeof acquireMissionWriter;
  custody: typeof dispatcherCustody;
  transport: typeof createMeshTransport;
  now: () => number;
  warn: (event: MissionServiceEvent) => void;
}
const defaults: MissionServiceDependencies = {
  acquireWriter: acquireMissionWriter, custody: dispatcherCustody, transport: createMeshTransport,
  now: Date.now, warn: event => console.warn(`[mission-dispatcher] ${event}`),
};
async function pause(ms: number, signal: AbortSignal): Promise<void> {
  if (!signal.aborted) await delay(ms, undefined, { signal }).catch(error => { if (!signal.aborted) throw error; });
}

/** One process, one identity, one SDK. Durable claims, not the polling loop, prevent resends. */
export class MissionDispatchService {
  private readonly deps: MissionServiceDependencies;
  private started = false;
  private stopping = false;
  private fatal = false;
  private verifiedAt: number | null = null;
  private anchor: DispatcherCustody | undefined;
  private checking: Promise<boolean> | undefined;
  private mesh: IMeshTransport | undefined;
  private active: MissionCandidate | undefined;

  constructor(private readonly api: KubernetesJson, private readonly config: MissionServiceConfig,
    dependencies: Partial<MissionServiceDependencies> = {}) {
    this.deps = { ...defaults, ...dependencies };
    if (!Number.isSafeInteger(config.pollMs) || config.pollMs < 100 || config.pollMs > 30_000
      || !Number.isSafeInteger(config.timeoutMs) || config.timeoutMs < 1_000 || config.timeoutMs > 300_000) {
      throw new Error("Invalid mission dispatcher timing configuration");
    }
  }

  ready(): boolean {
    return !this.stopping && !this.fatal && this.mesh?.isConnected === true && this.verifiedAt !== null
      && this.deps.now() - this.verifiedAt < 45_000;
  }

  private current(): Promise<boolean> {
    if (this.fatal) return Promise.resolve(false);
    return this.checking ??= this.checkCustody().finally(() => { this.checking = undefined; });
  }

  private async checkCustody(): Promise<boolean> {
    try {
      const next = await this.deps.custody(this.api, this.config.location, this.config.root);
      if (this.anchor && !isDeepStrictEqual(this.anchor, next)) {
        this.fatal = true;
        throw new Error("Dispatcher identity custody changed");
      }
      this.anchor ??= next;
      this.verifiedAt = this.deps.now();
      return true;
    } catch {
      this.verifiedAt = null;
      this.deps.warn("custody-unavailable");
      return false;
    }
  }

  private async monitor(signal: AbortSignal): Promise<void> {
    while (!signal.aborted && !this.fatal) {
      await pause(Math.min(this.config.pollMs, 5_000), signal);
      if (!signal.aborted) await this.current();
    }
  }

  async run(signal: AbortSignal): Promise<void> {
    if (this.started) throw new Error("Mission dispatcher has already started; restart the process");
    this.started = true;
    if (signal.aborted) { this.stopping = true; return; }
    const writer = await this.deps.acquireWriter();
    const monitoring = new AbortController();
    let monitor: Promise<void> | undefined;
    const stop = () => { this.stopping = true; monitoring.abort(); };
    signal.addEventListener("abort", stop, { once: true });
    if (signal.aborted) stop();
    try {
      while (!this.stopping && !await this.current()) await pause(this.config.pollMs, signal);
      if (this.stopping) return;
      const identity = missionIdentity(this.config.root, "dispatcher", this.anchor!.deploymentUid, this.anchor!.podUid);
      this.mesh = await this.deps.transport({
        identity, relayUrl: this.config.relayUrl, registryUrl: this.config.registryUrl,
        capabilities: ["kars:mission-dispatch"], displayName: "Kars mission dispatcher",
      });
      const admission = new KubernetesMissionStore(this.api, () => this.current());
      const store = new KubernetesMissionStore(this.api, async () => !this.stopping && await this.current() && !this.stopping);
      this.mesh.onKnock(async from => {
        const active = this.active;
        if (!active || from !== active.agentDid || this.mesh?.isConnected !== true) return { accept: false };
        try {
          const current = await admission.isCurrent(active);
          return { accept: current && this.active === active && this.mesh.isConnected };
        } catch { return { accept: false }; }
      });
      this.mesh.onError(() => this.deps.warn("transport-error"));
      this.mesh.onDisconnect(() => { this.verifiedAt = null; });
      if (this.stopping) return;
      await this.mesh.connect();
      if (this.mesh.currentDid !== identity.did || this.mesh.getPlaintextPeers().length) {
        throw new Error("Mission dispatcher transport identity or encryption configuration is invalid");
      }
      monitor = this.monitor(monitoring.signal);
      const dispatcher = new MissionDispatcher(this.mesh, store, randomUUID(), this.config.timeoutMs);
      while (!this.stopping && !this.fatal) {
        try {
          for await (const reference of store.pendingTasks()) {
            if (this.stopping || this.fatal) break;
            try {
              const candidate = await store.candidate(reference, identity.did);
              if (!candidate || this.stopping || this.fatal) continue;
              this.active = candidate;
              await dispatcher.dispatch(candidate);
            } catch { this.deps.warn("candidate-unavailable"); }
            finally { this.active = undefined; }
          }
        } catch { this.deps.warn("discovery-unavailable"); }
        if (!this.fatal) await pause(this.config.pollMs, signal);
      }
      if (this.fatal) throw new Error("Dispatcher identity custody changed; restart the process");
    } finally {
      this.stopping = true;
      this.verifiedAt = null;
      monitoring.abort();
      signal.removeEventListener("abort", stop);
      await monitor;
      // A failed disconnect deliberately retains the writer until process death.
      await this.mesh?.disconnect();
      await writer.close();
    }
  }
}
