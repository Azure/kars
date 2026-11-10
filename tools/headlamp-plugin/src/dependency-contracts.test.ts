// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
// @vitest-environment node

import { createRequire } from "node:module";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const require = createRequire(import.meta.url);
const from = (name: string) => createRequire(require.resolve(name));

describe("patched development dependency contracts", () => {
  it("preserves the named matcher used by TypeScript ESLint and the legacy callable matcher", () => {
    const { minimatch } = from("@typescript-eslint/typescript-estree")("minimatch");
    expect(minimatch(".config.ts", "*.ts", { dot: true })).toBe(true);
    expect(minimatch("src/index.tsx", "src/**/*.{ts,tsx}", { dot: true })).toBe(true);
    expect(minimatch("src/index.js", "src/**/*.{ts,tsx}", { dot: true })).toBe(false);
    const legacy = from("eslint")("minimatch");
    expect(typeof legacy).toBe("function");
    expect(legacy("src/index.tsx", "src/**/*.tsx")).toBe(true);
  });

  it("preserves the synchronous docgen and asynchronous rimraf glob APIs", async () => {
    const docgenGlob = from("@joshwooding/vite-plugin-react-docgen-typescript")("glob");
    const rimraf = createRequire(from("quick-temp").resolve("rimraf"));
    const rimrafGlob = rimraf("glob");
    const options = { cwd: process.cwd(), absolute: true, withFileTypes: false };
    const expected = [resolve("package-lock.json"), resolve("package.json")].sort();
    expect(docgenGlob.globSync("package{,-lock}.json", options).sort()).toEqual(expected);
    expect((await rimrafGlob.glob("package{,-lock}.json", options)).sort()).toEqual(expected);
    expect(rimrafGlob.globSync("package{,-lock}.json", options).sort()).toEqual(expected);
    await expect(rimrafGlob.glob("**/*", {
      ...options,
      signal: AbortSignal.abort(new Error("cancelled traversal")),
    })).rejects.toThrow("cancelled traversal");
  });

  it("supports Storybook's middleware lifecycle and rejects output-directory traversal", async () => {
    const builder = from("@storybook/builder-webpack5");
    const webpack = builder("webpack");
    const devMiddleware = builder("webpack-dev-middleware");
    const outputPath = resolve(".dependency-contract-output");
    const compiler = webpack({
      mode: "development",
      entry: {},
      output: { path: outputPath, publicPath: "/assets" },
      infrastructureLogging: { level: "none" },
    });
    // Keep compilation in memfs: the test never writes output to the checkout.
    const middleware = devMiddleware(compiler, {
      publicPath: "/assets",
      writeToDisk: false,
      stats: "errors-only",
    });
    try {
      expect(typeof middleware).toBe("function");
      expect(typeof middleware.invalidate).toBe("function");
      const stats = await new Promise<{ hasErrors(): boolean }>((done) => middleware.waitUntilValid(done));
      expect(stats.hasErrors()).toBe(false);
      const output = middleware.context.outputFileSystem;
      output.mkdirSync(outputPath, { recursive: true });
      output.writeFileSync(resolve(outputPath, "main.js"), "module.exports = 1;");
      output.writeFileSync(resolve(outputPath, "../secret.txt"), "outside output");
      expect(middleware.getFilenameFromUrl("/assets/main.js")).toBe(resolve(outputPath, "main.js"));
      expect(middleware.getFilenameFromUrl("/assets../secret.txt")).toBeUndefined();
    } finally {
      await new Promise<void>((done, reject) => middleware.close((error: Error | null) => error ? reject(error) : done()));
      await new Promise<void>((done, reject) => compiler.close((error: Error | null) => error ? reject(error) : done()));
    }
  });
});
