/**
 * Owner Dashboard - Local HTTP API for monitoring and control.
 * Binds to localhost only for security.
 */

import { createServer, IncomingMessage, ServerResponse, Server } from 'http';
import { AgentMeshClient, CircuitState } from '../client';
import { Policy } from '../config';

/**
 * Dashboard configuration.
 */
export interface DashboardConfig {
  /** Port to listen on (default: 3847) */
  port?: number;
  /** API key for authentication (optional) */
  apiKey?: string;
  /** Enable CORS for localhost origins (default: true) */
  cors?: boolean;
}

/**
 * Dashboard event types.
 */
export type DashboardEventType = 'dashboard_started' | 'dashboard_stopped' | 'dashboard_error';

/**
 * Event handler type.
 */
type EventHandler = (event: { type: DashboardEventType; data?: unknown }) => void;

/**
 * Dashboard error.
 */
export class DashboardError extends Error {
  readonly code: string;

  constructor(message: string, code: string) {
    super(message);
    this.name = 'DashboardError';
    this.code = code;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/**
 * Owner Dashboard for local monitoring and control.
 */
export class Dashboard {
  private readonly client: AgentMeshClient;
  private readonly port: number;
  private readonly apiKey?: string;
  private readonly corsEnabled: boolean;
  private server: Server | null = null;
  private eventHandlers: EventHandler[] = [];
  private running: boolean = false;

  constructor(client: AgentMeshClient, config: DashboardConfig = {}) {
    this.client = client;
    this.port = config.port ?? 3847;
    this.apiKey = config.apiKey;
    this.corsEnabled = config.cors ?? true;
  }

  /**
   * Check if dashboard is running.
   */
  get isRunning(): boolean {
    return this.running;
  }

  /**
   * Start the dashboard server.
   */
  async start(): Promise<void> {
    if (this.running) {
      throw new DashboardError('Dashboard already running', 'ALREADY_RUNNING');
    }

    return new Promise((resolve, reject) => {
      this.server = createServer((req, res) => this.handleRequest(req, res));

      this.server.on('error', (err: NodeJS.ErrnoException) => {
        if (err.code === 'EADDRINUSE') {
          reject(new DashboardError(`Port ${this.port} is already in use`, 'PORT_IN_USE'));
        } else {
          reject(new DashboardError(err.message, 'SERVER_ERROR'));
        }
      });

      // Bind to localhost only for security
      this.server.listen(this.port, '127.0.0.1', () => {
        this.running = true;
        this.emitEvent('dashboard_started', { port: this.port });
        resolve();
      });
    });
  }

  /**
   * Stop the dashboard server.
   */
  async stop(): Promise<void> {
    if (!this.running || !this.server) {
      return;
    }

    return new Promise((resolve) => {
      this.server!.close(() => {
        this.running = false;
        this.server = null;
        this.emitEvent('dashboard_stopped', {});
        resolve();
      });
    });
  }

  /**
   * Register an event handler.
   */
  onEvent(handler: EventHandler): void {
    this.eventHandlers.push(handler);
  }

  /**
   * Handle incoming HTTP request.
   */
  private async handleRequest(req: IncomingMessage, res: ServerResponse): Promise<void> {
    // Set CORS headers if enabled
    if (this.corsEnabled) {
      const origin = req.headers.origin;
      if (origin && (origin.startsWith('http://localhost') || origin.startsWith('http://127.0.0.1'))) {
        res.setHeader('Access-Control-Allow-Origin', origin);
        res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
        res.setHeader('Access-Control-Allow-Headers', 'Content-Type, X-API-Key');
      }
    }

    // Handle preflight
    if (req.method === 'OPTIONS') {
      res.writeHead(204);
      res.end();
      return;
    }

    // Check API key if configured
    if (this.apiKey) {
      const providedKey = req.headers['x-api-key'];
      if (providedKey !== this.apiKey) {
        this.sendJSON(res, 401, { error: 'Unauthorized', code: 'UNAUTHORIZED' });
        return;
      }
    }

    const url = req.url || '/';
    const method = req.method || 'GET';

    try {
      // Route handling
      if (url === '/status' && method === 'GET') {
        await this.handleGetStatus(res);
      } else if (url === '/sessions' && method === 'GET') {
        await this.handleGetSessions(res);
      } else if (url.startsWith('/sessions/') && url.endsWith('/kill') && method === 'POST') {
        const amid = url.slice('/sessions/'.length, -'/kill'.length);
        await this.handleKillSession(res, amid);
      } else if (url === '/policy' && method === 'GET') {
        await this.handleGetPolicy(res);
      } else if (url === '/policy' && method === 'POST') {
        const body = await this.readBody(req);
        await this.handleSetPolicy(res, body);
      } else if (url === '/circuit/pause' && method === 'POST') {
        await this.handleCircuitPause(res);
      } else if (url === '/circuit/resume' && method === 'POST') {
        await this.handleCircuitResume(res);
      } else if (url === '/circuit/emergency-stop' && method === 'POST') {
        await this.handleEmergencyStop(res);
      } else {
        this.sendJSON(res, 404, { error: 'Not found', code: 'NOT_FOUND' });
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Internal server error';
      this.sendJSON(res, 500, { error: message, code: 'INTERNAL_ERROR' });
      this.emitEvent('dashboard_error', { error });
    }
  }

  /**
   * GET /status - Get client status.
   */
  private async handleGetStatus(res: ServerResponse): Promise<void> {
    const info = this.client.getInfo();
    const rateLimitStatus = this.client.getRateLimitStatus();

    this.sendJSON(res, 200, {
      amid: info.amid,
      connected: info.connected,
      capabilities: info.capabilities,
      activeSessions: info.activeSessions,
      circuitState: info.circuitState,
      circuitStateChangedAt: info.circuitStateChangedAt,
      rateLimit: rateLimitStatus,
    });
  }

  /**
   * GET /sessions - Get active sessions.
   */
  private async handleGetSessions(res: ServerResponse): Promise<void> {
    const sessions = this.client.getSessions();

    this.sendJSON(res, 200, {
      count: sessions.length,
      sessions: sessions.map(s => ({
        id: s.id,
        remoteAmid: s.remoteAmid,
        state: s.state,
        isInitiator: s.isInitiator,
        createdAt: s.createdAt.toISOString(),
        lastActivity: s.lastActivity?.toISOString(),
        messagesSent: s.messagesSent,
        messagesReceived: s.messagesReceived,
      })),
    });
  }

  /**
   * POST /sessions/:amid/kill - Kill a session.
   */
  private async handleKillSession(res: ServerResponse, amid: string): Promise<void> {
    if (!amid) {
      this.sendJSON(res, 400, { error: 'AMID required', code: 'INVALID_REQUEST' });
      return;
    }

    await this.client.killSession(amid);
    this.sendJSON(res, 200, { success: true, amid });
  }

  /**
   * GET /policy - Get current policy.
   */
  private async handleGetPolicy(res: ServerResponse): Promise<void> {
    // Note: Policy is internal to the client, we expose what we can
    const info = this.client.getInfo();

    this.sendJSON(res, 200, {
      capabilities: info.capabilities,
      circuitState: info.circuitState,
    });
  }

  /**
   * POST /policy - Update policy.
   */
  private async handleSetPolicy(res: ServerResponse, body: string): Promise<void> {
    try {
      const data = JSON.parse(body);

      if (data.capabilities && Array.isArray(data.capabilities)) {
        await this.client.setCapabilities(data.capabilities);
      }

      if (data.policy) {
        const policy = new Policy(data.policy);
        this.client.setPolicy(policy);
      }

      this.sendJSON(res, 200, { success: true });
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Invalid request body';
      this.sendJSON(res, 400, { error: message, code: 'INVALID_REQUEST' });
    }
  }

  /**
   * POST /circuit/pause - Pause accepting new sessions.
   */
  private async handleCircuitPause(res: ServerResponse): Promise<void> {
    try {
      this.client.pauseNew();
      this.sendJSON(res, 200, { success: true, state: CircuitState.PAUSED });
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Failed to pause';
      this.sendJSON(res, 400, { error: message, code: 'PAUSE_FAILED' });
    }
  }

  /**
   * POST /circuit/resume - Resume accepting new sessions.
   */
  private async handleCircuitResume(res: ServerResponse): Promise<void> {
    try {
      this.client.resumeNew();
      this.sendJSON(res, 200, { success: true, state: CircuitState.RUNNING });
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Failed to resume';
      this.sendJSON(res, 400, { error: message, code: 'RESUME_FAILED' });
    }
  }

  /**
   * POST /circuit/emergency-stop - Emergency stop (terminal).
   */
  private async handleEmergencyStop(res: ServerResponse): Promise<void> {
    await this.client.emergencyStop();
    this.sendJSON(res, 200, { success: true, state: CircuitState.STOPPED });
  }

  /**
   * Read request body.
   */
  private async readBody(req: IncomingMessage): Promise<string> {
    return new Promise((resolve, reject) => {
      let body = '';
      req.on('data', (chunk) => {
        body += chunk.toString();
        // Limit body size to 1MB
        if (body.length > 1024 * 1024) {
          reject(new Error('Request body too large'));
        }
      });
      req.on('end', () => resolve(body));
      req.on('error', reject);
    });
  }

  /**
   * Send JSON response.
   */
  private sendJSON(res: ServerResponse, status: number, data: unknown): void {
    res.writeHead(status, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(data));
  }

  /**
   * Emit an event.
   */
  private emitEvent(type: DashboardEventType, data: unknown): void {
    for (const handler of this.eventHandlers) {
      try {
        handler({ type, data });
      } catch {
        // Ignore handler errors
      }
    }
  }
}
