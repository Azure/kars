import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["specs/**/*.spec.ts"],
    environment: "node",
    globals: false,
    reporters: ["default"],
    // Conformance tests exercise canonical crypto/protocol invariants; fork isolation
    // keeps ratchet / session state from leaking between spec files.
    pool: "forks",
  },
});
