const path = "/opt/azureclaw-plugin/node_modules/@agentmesh/sdk/dist/index.cjs";
const sdk = require(path);
// Use Node.js 22+ built-in WebSocket (same as SDK uses)

(async () => {
  console.log("SDK version:", sdk.VERSION);

  const identity = await sdk.Identity.generate();
  console.log("AMID:", identity.amid);
  console.log("PubKey b64 raw:", identity.signingPublicKeyB64Raw);
  console.log("PubKey raw len:", identity.getSigningPublicKeyRaw().length);

  // Test signTimestamp
  const [ts, sig] = await identity.signTimestamp();
  console.log("Timestamp:", ts);
  console.log("Signature:", sig, "len:", sig.length);

  // Verify locally using SDK
  const localOk = await sdk.Identity.verifySignature(
    identity.signingPublicKeyB64Raw,
    new TextEncoder().encode(ts),
    sig
  );
  console.log("SDK local verify:", localOk);

  // Now try connecting directly via the SDK's AgentMeshClient
  console.log("\n--- Test: SDK AgentMeshClient.connect() via proxy ---");
  const client = new sdk.AgentMeshClient(identity, {
    storage: new sdk.MemoryStorage(),
    registryUrl: "http://127.0.0.1:8443/agt/registry",
    relayUrl: "ws://127.0.0.1:8443/agt/relay",
  });

  try {
    await Promise.race([
      client.connect({
        displayName: "test-agent",
        capabilities: ["test"],
      }),
      new Promise((_, reject) => setTimeout(() => reject(new Error("Connect timeout 15s")), 15000))
    ]);
    console.log("CONNECTED SUCCESSFULLY!");
  } catch (e) {
    console.error("Connect failed:", e.message);
  }

  // Also test direct WebSocket to see the raw exchange
  console.log("\n--- Test: Manual WS with SDK-generated auth ---");
  const ws = new WebSocket("ws://127.0.0.1:8443/agt/relay");
  ws.on("open", async () => {
    console.log("WS opened");
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
  });
  ws.on("message", (data) => { console.log("Response:", data.toString()); });
  ws.on("error", (e) => { console.error("WS error:", e.message); });
  ws.on("close", (code, reason) => {
    console.log("WS closed, code:", code, "reason:", reason?.toString());
    process.exit(0);
  });

  setTimeout(() => { console.log("Timeout"); process.exit(1); }, 25000);
})();
