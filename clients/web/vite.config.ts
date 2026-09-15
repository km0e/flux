import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';

/** Vendor chunk groups — see the manualChunks comment in `build` below. */
const VENDOR_GROUPS: Array<[string, string[]]> = [
  [
    'vendor-react',
    ['react', 'react-dom', 'scheduler', 'zustand', '@babel/runtime', 'lucide-react'],
  ],
  ['vendor-rpc', ['@bufbuild/protobuf', '@connectrpc']],
  [
    'vendor-radix',
    [
      '@radix-ui',
      '@floating-ui',
      'react-remove-scroll',
      'aria-hidden',
      'react-style-singleton',
      'use-sidecar',
      'use-sync-external-store',
      'get-nonce',
    ],
  ],
  ['vendor-markdown', ['marked', 'dompurify']],
];

// The flux-server static layer (crates/flux-server/src/web.rs) serves the
// assets directory GENERICALLY: whatever the build emits under assets/ is
// served by name (tower-http ServeDir + on-the-fly gzip), with immutable
// caching — file names carry content hashes, so names change exactly when
// content does.
//
// The vendor groups: always-loaded third-party code, grouped by role and
// change cadence. `react` = the render core (rarely moves); `rpc` = the wire
// codec (moves with the proto surface only); `radix` = the control-primitive
// layer + its floating-ui/scroll-lock dependencies; `markdown` = the message
// rendering stack (marked's lexer + DOMPurify).
//
// Chunking follows the warning's own recipe, on two levels:
//
// 1. Source-level dynamic imports defer the non-critical weight: highlight.js
//    (~129 KB) loads as its own chunk, prefetched at bootstrap; the Files tree
//    (~134 KB) loads on first Files-tab activation; xterm (~329 KB) on first
//    terminal creation; the Settings dialog chain (~30 KB of app code) on
//    first gear click (React.lazy in TopBar).
// 2. manualChunks splits the ALWAYS-loaded third-party code into stable
//    vendor groups, so the entry keeps only first-party code. Two payoffs:
//    every chunk stays far below Vite's 500 KB default warning limit (the
//    raised `chunkSizeWarningLimit` this file used to carry is gone), and
//    each group's content hash changes only when that dependency does — an
//    app-only deploy re-downloads the entry, not the whole graph. Groups are
//    leaves in the import graph (vendor code never imports app code), so no
//    chunk-cycle risk.
//
// One CSS file stays (cssCodeSplit: false).
export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Dev workflow: `npm run dev` serves the UI on Vite's own port and
  // proxies the same-origin /ws upgrade to the flux-server listener.
  server: {
    proxy: {
      '/ws': {
        target: 'http://127.0.0.1:8080',
        ws: true,
      },
    },
  },
  build: {
    target: 'es2022',
    cssCodeSplit: false,
    // Size reporting recomputes gzip per artifact — build speed over logs.
    reportCompressedSize: false,
    // Content-hashed names are what make immutable caching correct.
    rollupOptions: {
      output: {
        entryFileNames: 'assets/[name]-[hash].js',
        chunkFileNames: 'assets/[name]-[hash].js',
        assetFileNames: 'assets/[name]-[hash][extname]',
        // Vendor groups: package name → chunk. Matching is package-boundary
        // exact (`react` never catches `react-dom`; `@radix-ui` catches
        // every @radix-ui/react-* package). Anything unlisted stays where
        // the import graph puts it (e.g. react-window rides the Explorer
        // chunk). tsconfig/es2022 target, one pass — cost is negligible.
        manualChunks(id) {
          const marker = id.lastIndexOf('node_modules/');
          if (marker === -1) return undefined;
          const rest = id.slice(marker + 'node_modules/'.length);
          // Scoped package: @scope/name — both segments belong to the name.
          const pkg = rest.startsWith('@')
            ? rest.split('/').slice(0, 2).join('/')
            : rest.split('/')[0];
          for (const [chunk, members] of VENDOR_GROUPS) {
            for (const m of members) {
              if (pkg === m || pkg.startsWith(m + '/')) return chunk;
            }
          }
          return undefined;
        },
      },
    },
  },
  test: {
    environment: 'jsdom',
    globals: false,
    setupFiles: ['./src/test/setup.ts'],
    include: ['src/__tests__/**/*.test.ts', 'src/__tests__/**/*.test.tsx'],
    coverage: {
      provider: 'v8',
      // Generated wire contracts are import-only (never hand-written);
      // test fixtures and static assets carry nothing to measure.
      include: ['src/**'],
      exclude: ['src/gen/**', 'src/test/**', 'src/assets/**', 'src/**/__tests__/**'],
      reporter: ['text-summary'],
    },
  },
});
