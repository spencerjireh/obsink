import { defineConfig } from 'vitest/config'

// The shared screens under jsdom with a mocked Backend (no wasm, no Tauri).
export default defineConfig({
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.tsx'],
    globals: false,
  },
  esbuild: { jsx: 'automatic' },
})
