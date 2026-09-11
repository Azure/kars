import type { NextConfig } from "next";

// NOTE: the same-origin /api/* proxy to the BFF is handled at RUNTIME in
// src/proxy.ts (middleware), NOT via a next.config `rewrites` — a standalone
// build freezes a rewrite destination at build time (when BRIDGE_BFF_URL is
// unset), which would pin every deployment to localhost:8081. Middleware reads
// BRIDGE_BFF_URL per-request so the one image works on kind/AKS/EKS/GKE.
const nextConfig: NextConfig = {
  // Emit a self-contained server bundle (server.js + minimal node_modules) so the
  // container image is small and needs no full install at runtime.
  output: "standalone",
};

export default nextConfig;
