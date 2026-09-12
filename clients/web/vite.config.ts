import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';

// The flux-server static layer (crates/flux-server/src/web.rs) serves the
// assets directory GENERICALLY: whatever the build emits under assets/ is
// served by name (tower-http ServeDir + on-the-fly gzip), with immutable
// caching — file names carry content hashes, so names change exactly when
// content does.
//
// Chunking follows the warning's own recipe: source-level dynamic imports
// defer the non-critical weight (highlight.js ~200 KB min loads as its own
// chunk, prefetched at bootstrap; the Files tree loads on first Files-tab
// activation), and one CSS file stays (cssCodeSplit: false).
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
    // The entry sits just over Vite's 500 kB default (the dock shell +
    // Radix); the heavy libs are already split (xterm/hljs/Explorer lazy
    // chunks) — the limit is noise for a served-locally single-page app.
    chunkSizeWarningLimit: 600,
    // Content-hashed names are what make immutable caching correct.
    rollupOptions: {
      output: {
        entryFileNames: 'assets/[name]-[hash].js',
        chunkFileNames: 'assets/[name]-[hash].js',
        assetFileNames: 'assets/[name]-[hash][extname]',
      },
    },
  },
  test: {
    environment: 'jsdom',
    globals: false,
    setupFiles: ['./src/test/setup.ts'],
    include: ['src/__tests__/**/*.test.ts', 'src/__tests__/**/*.test.tsx'],
  },});
