const sdk = require("/opt/azureclaw-plugin/node_modules/@agentmesh/sdk/dist/index.cjs");

(async () => {
  console.log("SDK version:", sdk.VERSION);
  const identity = await sdk.Identity.generate();
  console.log("AMID:", identity.amid);
  console.log("PubKey b64 raw:", identity.signingPublicKeyB64Raw);
  console.log("PubKey raw len:", identity.getSigningPublicKeyRaw().length);

  const [ts, sig] = await identity.signTimestamp();
  console.log("Timestamp:", ts);
  console.log("Signature:", sig, "len:", sig.length);

  const localOk = await sdk.Identity.verifySignature(
    identity.signingPublicKeyB64Raw,
    new TextEncoder().encode(ts),
    sig
  );
  console.log("SDK local verify:", localOk);

  // Test 1: SDK AgentMeshClient
  console.log("\n--- Test 1: SDK AgentMeshClient.connect() ---");
  const client = new sdk.AgentMeshClient(identity, {
    storage: new sdk.MemoryStorage(),
    registryUrl: "http://127.0.0.1:8443/agt/registry",
    relayUrl: "ws://127.0.0.1:8443/agt/relay",
  });
  try {
    await Promise.race([
      client.connect({ displayName: "test-agent", capabilities: ["test"] }),
      new Promise((_, reject) => setTimeout(() => reject(new Error("Connect timeout 15s")), 15000))
    ]);
    console.log("CONNECTED SUCCESSFULLY!");
  } catch (e) {
    console.error("Connect failed:", e.message);
  }

  // Test 2: Manual WS with SDK-generated identity/auth
  console.log("\n--- Test 2: Manual WS with SDK auth ---");
  const ws = new WebSocket("ws://127.0.0.1:8443/agt/relay");
  ws.onopen = async () => {
    console.log("WS opened to proxy");
    const [ts2, sig2] = await identity.signTimestamp();
    const msg = JSON.stringify({
      type: "connect",
      protocol: "agentmesh/0.2",
      amid: identity.amid,
      public_key: identity.signingPublicKeyB64Raw,
      signature: sig2,
      timestamp: ts2,
      p2p_capable: false
    });
    console.log("Sending:", msg);
    ws.send(msg);
  };
  ws.onmessage = (event) => { console.log("Response:", typeof event.data === 'string' ? event.data : event.data.toString()); };
  ws.onerror = (event) => { console.error("WS error:", event); };
  ws.onclose = (event) => {
    console.log("WS closed, code:", event.code, "reason:", event.reason);
    process.exit(0);
  };
  setTimeout(() => { console.log("Timeout 25s"); process.exit(1); }, 25000);
})();
