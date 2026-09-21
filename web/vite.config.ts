import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

// Served at /app by the web container; in dev the API paths are proxied to a
// local server so the client can stay same-origin (OBSINK_DEV_SERVER_URL).
const devServer = process.env.OBSINK_DEV_SERVER_URL ?? 'http://localhost:18080'
const wasmPkg = fileURLToPath(new URL('../core-wasm/pkg', import.meta.url))

export default defineConfig({
  base: '/app/',
  plugins: [react()],
  resolve: {
    // The wasm-pack output of core-wasm (built by `wasm-pack build core-wasm --target web`).
    alias: { '@obsink/core-wasm': wasmPkg },
  },
  optimizeDeps: { exclude: ['@obsink/core-wasm'] },
  worker: { format: 'es' },
  build: { target: 'es2022' },
  server: {
    proxy: {
      '^/$': devServer,
      '/auth': devServer,
      '/vaults': devServer,
      '/healthz': devServer,
    },
  },
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
  clearScreen: false,
})
