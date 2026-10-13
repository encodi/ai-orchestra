/// <reference types="vitest/config" />
import { defineConfig, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'

// Development only: `npm run dev` serves the frontend with hot reload and proxies /api to a running
// `ai-orchestra web`. That server puts its per-run token in index.html, so the dev server reads it from
// there; the proxy presents the target's own Host and Origin so the server's checks stay switched on.
const TARGET = process.env.AIO_TARGET ?? 'http://127.0.0.1:8765'

function devToken(): Plugin {
  return {
    name: 'aio-dev-token',
    apply: 'serve',
    async transformIndexHtml(html) {
      try {
        const text = await (await fetch(`${TARGET}/`)).text()
        const m = /name="aio-token" content="([0-9a-f]+)"/.exec(text)
        return m ? html.replace('__AIO_TOKEN__', m[1] ?? '') : html
      } catch {
        return html
      }
    },
  }
}

// The compiled files in `dist/` are embedded in the Rust binary by `build.rs`.
export default defineConfig({
  plugins: [react(), devToken()],
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: false, target: 'es2022', chunkSizeWarningLimit: 400 },
  server: {
    port: 5173,
    proxy: {
      '/api': {
        target: TARGET,
        changeOrigin: true,
        configure: (proxy) => proxy.on('proxyReq', (req) => req.setHeader('origin', TARGET)),
      },
    },
  },
  test: { environment: 'jsdom', globals: true, setupFiles: ['./tests/setup.ts'], include: ['tests/**/*.test.{ts,tsx}'] },
})
