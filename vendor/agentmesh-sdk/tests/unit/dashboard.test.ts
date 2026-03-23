/**
 * Unit tests for Dashboard module.
 */
import { describe, test, expect, vi, beforeEach, afterEach } from 'vitest';
import { Dashboard, DashboardError } from '../../src/dashboard';
import { AgentMeshClient, CircuitState } from '../../src/client';
import http from 'http';

// Mock the AgentMeshClient
const createMockClient = () => ({
  getInfo: vi.fn().mockReturnValue({
    amid: 'test-amid',
    connected: true,
    capabilities: ['test/capability'],
    activeSessions: 2,
    circuitState: CircuitState.RUNNING,
    circuitStateChangedAt: Date.now(),
    registryUrl: 'https://test.registry',
    relayUrl: 'wss://test.relay',
  }),
  getSessions: vi.fn().mockReturnValue([
    {
      id: 'session-1',
      remoteAmid: 'peer-1',
      state: 'ACTIVE',
      isInitiator: true,
      createdAt: new Date(),
      lastActivity: new Date(),
      messagesSent: 5,
      messagesReceived: 3,
    },
  ]),
  getRateLimitStatus: vi.fn().mockReturnValue({
    tokens: 100,
    maxTokens: 500,
    refillRate: 100,
    peerStatuses: new Map(),
  }),
  killSession: vi.fn(),
  setCapabilities: vi.fn(),
  setPolicy: vi.fn(),
  pauseNew: vi.fn(),
  resumeNew: vi.fn(),
  emergencyStop: vi.fn(),
});

describe('Dashboard', () => {
  let dashboard: Dashboard;
  let mockClient: ReturnType<typeof createMockClient>;
  const testPort = 38470 + Math.floor(Math.random() * 100);

  beforeEach(() => {
    mockClient = createMockClient();
    dashboard = new Dashboard(mockClient as unknown as AgentMeshClient, { port: testPort });
  });

  afterEach(async () => {
    if (dashboard.isRunning) {
      await dashboard.stop();
    }
  });

  describe('constructor', () => {
    test('should create dashboard with default config', () => {
      const d = new Dashboard(mockClient as unknown as AgentMeshClient);
      expect(d).toBeDefined();
      expect(d.isRunning).toBe(false);
    });

    test('should create dashboard with custom config', () => {
      const d = new Dashboard(mockClient as unknown as AgentMeshClient, {
        port: 4000,
        apiKey: 'test-key',
        cors: false,
      });
      expect(d).toBeDefined();
    });
  });

  describe('start/stop', () => {
    test('should start and stop dashboard', async () => {
      expect(dashboard.isRunning).toBe(false);

      await dashboard.start();
      expect(dashboard.isRunning).toBe(true);

      await dashboard.stop();
      expect(dashboard.isRunning).toBe(false);
    });

    test('should throw when starting already running dashboard', async () => {
      await dashboard.start();

      await expect(dashboard.start()).rejects.toThrow(DashboardError);
    });

    test('should emit events on start/stop', async () => {
      const handler = vi.fn();
      dashboard.onEvent(handler);

      await dashboard.start();
      expect(handler).toHaveBeenCalledWith({
        type: 'dashboard_started',
        data: { port: testPort },
      });

      await dashboard.stop();
      expect(handler).toHaveBeenCalledWith({
        type: 'dashboard_stopped',
        data: {},
      });
    });
  });

  describe('HTTP endpoints', () => {
    beforeEach(async () => {
      await dashboard.start();
      // Wait for server to be ready
      await new Promise(resolve => setTimeout(resolve, 50));
    });

    const makeRequest = (
      method: string,
      path: string,
      body?: unknown,
      headers?: Record<string, string>
    ): Promise<{ status: number; data: unknown }> => {
      return new Promise((resolve, reject) => {
        const options = {
          hostname: '127.0.0.1',
          port: testPort,
          path,
          method,
          headers: {
            'Content-Type': 'application/json',
            ...headers,
          },
        };

        const req = http.request(options, (res) => {
          let data = '';
          res.on('data', (chunk) => (data += chunk));
          res.on('end', () => {
            try {
              resolve({
                status: res.statusCode || 0,
                data: data ? JSON.parse(data) : null,
              });
            } catch {
              resolve({ status: res.statusCode || 0, data });
            }
          });
        });

        req.on('error', reject);

        if (body) {
          req.write(JSON.stringify(body));
        }
        req.end();
      });
    };

    test('GET /status should return client status', async () => {
      const { status, data } = await makeRequest('GET', '/status');

      expect(status).toBe(200);
      expect(data).toMatchObject({
        amid: 'test-amid',
        connected: true,
        capabilities: ['test/capability'],
        activeSessions: 2,
        circuitState: 'RUNNING',
      });
    });

    test('GET /sessions should return active sessions', async () => {
      const { status, data } = await makeRequest('GET', '/sessions');

      expect(status).toBe(200);
      expect((data as { count: number }).count).toBe(1);
      expect((data as { sessions: unknown[] }).sessions).toHaveLength(1);
    });

    test('POST /sessions/:amid/kill should kill session', async () => {
      const { status, data } = await makeRequest('POST', '/sessions/peer-1/kill');

      expect(status).toBe(200);
      expect((data as { success: boolean }).success).toBe(true);
      expect(mockClient.killSession).toHaveBeenCalledWith('peer-1');
    });

    test('GET /policy should return policy info', async () => {
      const { status, data } = await makeRequest('GET', '/policy');

      expect(status).toBe(200);
      expect(data).toMatchObject({
        capabilities: ['test/capability'],
        circuitState: 'RUNNING',
      });
    });

    test('POST /policy should update policy', async () => {
      const { status, data } = await makeRequest('POST', '/policy', {
        capabilities: ['new/capability'],
      });

      expect(status).toBe(200);
      expect((data as { success: boolean }).success).toBe(true);
      expect(mockClient.setCapabilities).toHaveBeenCalledWith(['new/capability']);
    });

    test('POST /circuit/pause should pause circuit', async () => {
      const { status, data } = await makeRequest('POST', '/circuit/pause');

      expect(status).toBe(200);
      expect((data as { success: boolean }).success).toBe(true);
      expect((data as { state: string }).state).toBe('PAUSED');
      expect(mockClient.pauseNew).toHaveBeenCalled();
    });

    test('POST /circuit/resume should resume circuit', async () => {
      const { status, data } = await makeRequest('POST', '/circuit/resume');

      expect(status).toBe(200);
      expect((data as { success: boolean }).success).toBe(true);
      expect(mockClient.resumeNew).toHaveBeenCalled();
    });

    test('POST /circuit/emergency-stop should trigger emergency stop', async () => {
      const { status, data } = await makeRequest('POST', '/circuit/emergency-stop');

      expect(status).toBe(200);
      expect((data as { success: boolean }).success).toBe(true);
      expect((data as { state: string }).state).toBe('STOPPED');
      expect(mockClient.emergencyStop).toHaveBeenCalled();
    });

    test('should return 404 for unknown routes', async () => {
      const { status, data } = await makeRequest('GET', '/unknown');

      expect(status).toBe(404);
      expect((data as { code: string }).code).toBe('NOT_FOUND');
    });
  });

  describe('API key authentication', () => {
    beforeEach(async () => {
      dashboard = new Dashboard(mockClient as unknown as AgentMeshClient, {
        port: testPort + 1,
        apiKey: 'secret-key',
      });
      await dashboard.start();
      // Wait for server to be ready
      await new Promise(resolve => setTimeout(resolve, 50));
    });

    afterEach(async () => {
      await dashboard.stop();
    });

    const makeRequest = (
      path: string,
      apiKey?: string
    ): Promise<{ status: number; data: unknown }> => {
      return new Promise((resolve, reject) => {
        const headers: Record<string, string> = {
          'Content-Type': 'application/json',
        };
        if (apiKey) {
          headers['X-API-Key'] = apiKey;
        }

        const options = {
          hostname: '127.0.0.1',
          port: testPort + 1,
          path,
          method: 'GET',
          headers,
        };

        const req = http.request(options, (res) => {
          let data = '';
          res.on('data', (chunk) => (data += chunk));
          res.on('end', () => {
            resolve({
              status: res.statusCode || 0,
              data: data ? JSON.parse(data) : null,
            });
          });
        });

        req.on('error', reject);
        req.end();
      });
    };

    test('should reject request without API key', async () => {
      const { status, data } = await makeRequest('/status');

      expect(status).toBe(401);
      expect((data as { code: string }).code).toBe('UNAUTHORIZED');
    });

    test('should reject request with wrong API key', async () => {
      const { status, data } = await makeRequest('/status', 'wrong-key');

      expect(status).toBe(401);
      expect((data as { code: string }).code).toBe('UNAUTHORIZED');
    });

    test('should accept request with correct API key', async () => {
      const { status, data } = await makeRequest('/status', 'secret-key');

      expect(status).toBe(200);
      expect((data as { amid: string }).amid).toBe('test-amid');
    });
  });
});
