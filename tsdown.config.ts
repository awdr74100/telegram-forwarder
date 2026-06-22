import { defineConfig } from 'tsdown';

export default defineConfig({
  entry: ['src/cli.ts'],
  format: ['esm'],
  platform: 'node',
  outDir: 'dist',
  clean: true,
  dts: false,
  // Validates the published package.json (bin paths, files, type, etc.).
  publint: true,
  // attw checks type resolution for consumers importing the package. This is a
  // bin-only CLI with no library exports, so it has no types to check — enable
  // it (with `dts: true` + an `exports` map) only if a programmatic API is added.
  attw: false,
});
