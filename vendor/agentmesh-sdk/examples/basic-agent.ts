/**
 * Basic AgentMesh Agent Example
 *
 * This example shows how to create and initialize a simple agent,
 * handle incoming connections, and send messages.
 */

import {
  AgentMeshClient,
  MemoryStorage,
  Policy,
  SessionInfo,
} from '@agentmesh/sdk';

async function main() {
  // Create storage (use FileStorage for persistence)
  const storage = new MemoryStorage();

  // Create the agent client
  const agent = new AgentMeshClient({
    storage,
    policy: Policy.verified(), // Accept connections from verified agents
    registryUrl: 'https://agentmesh.online/v1',
    relayUrl: 'wss://relay.agentmesh.online/v1/connect',
  });

  // Initialize the agent (generates identity and prekeys)
  await agent.initialize();
  console.log('Agent initialized with AMID:', agent.amid);

  // Register message handler
  agent.onMessage((message) => {
    console.log('Received message from:', message.from);
    console.log('Payload:', message.payload);

    // Echo the message back
    if (message.sessionId) {
      agent.send(message.sessionId, {
        type: 'echo',
        originalMessage: message.payload,
        timestamp: Date.now(),
      }).catch(console.error);
    }
  });

  // Register KNOCK handler (incoming session requests)
  agent.onKnock(async (knock) => {
    console.log('Received KNOCK from:', knock.from);
    console.log('Intent:', knock.intent);

    // Accept the connection (policy already evaluated)
    return { accept: true };
  });

  // Connect to the network
  const connected = await agent.connect({
    displayName: 'Example Agent',
    capabilities: ['echo/message', 'demo/hello'],
  });

  if (connected) {
    console.log('Connected to AgentMesh network');
  } else {
    console.error('Failed to connect');
    return;
  }

  // Keep the agent running
  console.log('Agent is running. Press Ctrl+C to stop.');

  // Handle graceful shutdown
  process.on('SIGINT', async () => {
    console.log('\nShutting down...');
    await agent.shutdown();
    process.exit(0);
  });

  // Keep process alive
  await new Promise(() => {});
}

main().catch(console.error);
