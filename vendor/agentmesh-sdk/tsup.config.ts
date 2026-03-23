import { defineConfig } from 'tsup';

export default defineConfig({
  entry: {
    index: 'src/index.ts',
    identity: 'src/identity.ts',
    discovery: 'src/discovery.ts',
    transport: 'src/transport.ts',
    'storage/index': 'src/storage/index.ts',
  },
  format: ['esm', 'cjs'],
  dts: true,
  splitting: true,
  sourcemap: true,
  clean: true,
  treeshake: true,
  minify: false,
  target: 'es2022',
  outDir: 'dist',
  external: ['@cloudflare/workers-types'],
});
