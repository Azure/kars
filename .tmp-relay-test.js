const { WebSocket } = require("/usr/local/lib/node_modules/openclaw/node_modules/ws");

(async () => {
  const kp = await crypto.subtle.generateKey({name:"Ed25519"}, true, ["sign","verify"]);
  const pubRaw = new Uint8Array(await crypto.subtle.exportKey("raw", kp.publicKey));

  const hashBuf = await crypto.subtle.digest("SHA-256", pubRaw);
  const hashArr = new Uint8Array(hashBuf);
  const ALPHA = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let num = BigInt(0);
  for (const b of hashArr) num = num * 256n + BigInt(b);
  let amid = "";
  while (num > 0n) { amid = ALPHA[Number(num % 58n)] + amid; num = num / 58n; }

  function toB64(buf) { let s=""; for(const b of buf) s += String.fromCharCode(b); return btoa(s); }

  console.log("AMID:", amid);
  console.log("PubKey b64:", toB64(pubRaw), "len:", pubRaw.length);

  // Test via proxy (only path available due to NetworkPolicy)
  console.log("\n--- Test: Via proxy ws://127.0.0.1:8443/agt/relay ---");
  const ts = new Date().toISOString();
  const sig = new Uint8Array(await crypto.subtle.sign({name:"Ed25519"}, kp.privateKey, new TextEncoder().encode(ts)));
  const ok = await crypto.subtle.verify({name:"Ed25519"}, kp.publicKey, sig, new TextEncoder().encode(ts));
  console.log("Timestamp:", ts, "- Local verify:", ok, "- Sig len:", sig.length);

  const ws = new WebSocket("ws://127.0.0.1:8443/agt/relay");
  ws.on("open", () => {
    console.log("WS opened to proxy");
    const msg = JSON.stringify({
      type: "connect",
      protocol: "agentmesh/0.2",
      amid: amid,
      public_key: toB64(pubRaw),
      signature: toB64(sig),
      timestamp: ts,
      p2p_capable: false
    });
    console.log("Sending:", msg);
    ws.send(msg);
  });
  ws.on("message", (data) => {
    console.log("Response:", data.toString());
    ws.close();
  });
  ws.on("error", (e) => { console.error("WS error:", e.message); });
  ws.on("close", (code, reason) => {
    console.log("WS closed, code:", code, "reason:", reason?.toString());
    process.exit(0);
  });
  setTimeout(() => { console.log("Timeout - no response after 10s"); process.exit(1); }, 10000);
})();
