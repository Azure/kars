#!/usr/bin/env node
/**
 * AGT Relay Listener — persistent Node.js process that keeps the mesh connection
 * alive so the agent can receive E2E encrypted messages from other agents.
 *
 * Unlike `openclaw agent --local --message`, this process stays alive indefinitely.
 * When a task_request arrives, it spawns `openclaw agent --local --message` to
 * process the task with full toolset access, then sends the reply back via relay.
 */

import { createRequire } from 'node:module';
import http from 'node:http';
const require = createRequire(import.meta.url);

const RELAY_URL = process.env.AGT_RELAY_URL || '';
const REGISTRY_URL = process.env.AGT_REGISTRY_URL || '';
const SANDBOX_NAME = process.env.SANDBOX_NAME || process.env.HOSTNAME || 'unknown';
const ROUTER_URL = 'http://127.0.0.1:8443';

async function main() {
  // Wait for router to be ready
  for (let i = 0; i < 30; i++) {
    try {
      await new Promise((resolve, reject) => {
        const req = http.get(`${ROUTER_URL}/health`, (res) => {
          let d = ''; res.on('data', c => d += c); res.on('end', () => resolve(d));
        });
        req.on('error', reject);
        req.setTimeout(2000, () => { req.destroy(); reject(new Error('timeout')); });
      });
      break;
    } catch {
      await new Promise(r => setTimeout(r, 1000));
    }
  }

  // Load SDK
  let sdk;
  try {
    sdk = require('/sandbox/.openclaw/extensions/azureclaw/node_modules/@agentmesh/sdk');
  } catch {
    try {
      sdk = require('@agentmesh/sdk');
    } catch (e) {
      console.error('[agt-listener] SDK not found:', e.message);
      process.exit(1);
    }
  }

  const relayUrl = `ws://127.0.0.1:8443/agt/relay`;
  const registryUrl = `${ROUTER_URL}/agt/registry`;

  // Create identity and client — match plugin.ts proven code path exactly
  const identity = await sdk.Identity.generate();
  console.log(`[agt-listener] Identity generated (AMID: ${identity.amid})`);

  const client = new sdk.AgentMeshClient(identity, {
    storage: new sdk.MemoryStorage(),
    relayUrl,
    registryUrl,
  });

  // Connect with retry
  let connected = false;
  for (let attempt = 0; attempt < 10; attempt++) {
    try {
      await client.connect({
        displayName: SANDBOX_NAME,
        capabilities: ['azureclaw-agent', 'task-execution', SANDBOX_NAME],
      });
      connected = true;
      console.log(`[agt-listener] Mesh connected (AMID: ${client.amid}, relay: ${relayUrl})`);
      break;
    } catch (e) {
      console.warn(`[agt-listener] Connect attempt ${attempt + 1}/10 failed: ${e.message}`);
      await new Promise(r => setTimeout(r, 2000));
    }
  }

  if (!connected) {
    console.error('[agt-listener] Failed to connect to mesh after 10 attempts');
    process.exit(1);
  }

  // Handle incoming messages — spawn openclaw agent for each task
  client.onMessage(async (fromAmid, message) => {
    const content = typeof message === 'string' ? message
      : (message?.content || message?.text || JSON.stringify(message));
    const msgType = message?.type || 'unknown';
    const fromName = message?.from_agent || fromAmid.slice(0, 12);

    console.log(`[agt-listener] Message from ${fromName}: type=${msgType} content=${String(content).slice(0, 200)}`);

    // Skip protocol messages
    if (msgType === 'ACCEPT' || msgType === 'KNOCK' || msgType === 'KEY_EXCHANGE') return;

    if (msgType === 'task_request') {
      console.log(`[agt-listener] Processing task from ${fromName}...`);
      try {
        const taskText = typeof content === 'string' ? content : JSON.stringify(content);

        // Call inference router directly — no subprocess, no device fingerprint conflicts
        const llmResponse = await new Promise((resolve, reject) => {
          const body = JSON.stringify({
            model: process.env.AZURECLAW_MODEL || 'gpt-4.1',
            messages: [
              { role: 'system', content: `You are ${SANDBOX_NAME}, a sub-agent. Respond to the task concisely and helpfully.` },
              { role: 'user', content: taskText },
            ],
            max_tokens: 2048,
          });
          const req = http.request(`${ROUTER_URL}/v1/chat/completions`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) },
          }, (res) => {
            let data = '';
            res.on('data', c => data += c);
            res.on('end', () => {
              try {
                const json = JSON.parse(data);
                const text = json.choices?.[0]?.message?.content || '';
                if (text) resolve(text);
                else reject(new Error(`Empty LLM response: ${data.slice(0, 200)}`));
              } catch (e) {
                reject(new Error(`Failed to parse LLM response: ${e.message} — ${data.slice(0, 200)}`));
              }
            });
          });
          req.on('error', reject);
          req.setTimeout(60000, () => { req.destroy(); reject(new Error('LLM request timeout')); });
          req.write(body);
          req.end();
        });

        // Send reply via encrypted relay
        await client.send(fromAmid, {
          type: 'task_response',
          content: llmResponse,
          from_agent: SANDBOX_NAME,
          timestamp: new Date().toISOString(),
        });
        console.log(`[agt-listener] Reply sent to ${fromName} (${llmResponse.length} chars)`);

        // Push trust update to router
        try {
          const trustData = JSON.stringify({ agent_id: fromName, score: 800, interactions: 1 });
          await new Promise((resolve, reject) => {
            const req = http.request(`${ROUTER_URL}/agt/trust`, {
              method: 'POST', headers: { 'Content-Type': 'application/json' },
            }, (res) => { let d = ''; res.on('data', c => d += c); res.on('end', () => resolve(d)); });
            req.on('error', reject);
            req.write(trustData);
            req.end();
          });
        } catch { /* best effort */ }
      } catch (err) {
        console.error(`[agt-listener] Task failed: ${err.message}`);
        try {
          await client.send(fromAmid, {
            type: 'task_response',
            content: `Error processing task: ${err.message}`,
            from_agent: SANDBOX_NAME,
            timestamp: new Date().toISOString(),
          });
        } catch { /* best effort */ }
      }
    }
  });

  // Keepalive — prevents Node.js from exiting
  setInterval(() => {
    console.log(`[agt-listener] Alive (AMID: ${client.amid})`);
  }, 60000);

  console.log(`[agt-listener] Listening for mesh messages as '${SANDBOX_NAME}'`);
}

main().catch(err => {
  console.error('[agt-listener] Fatal:', err);
  process.exit(1);
});
