import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["specs/**/*.spec.ts"],
    environment: "node",
    globals: false,
    reporters: ["default"],
    pool: "forks",  // blessed mocks keep per-test module state; fork isolation = clean slate per file
  },
});
