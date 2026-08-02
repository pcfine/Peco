import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import path from 'path'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    port: 9233,
    proxy: {
      '/api': {
        target: 'http://localhost:9227',
        changeOrigin: true,
      },
      '/uploads': {
        target: 'http://localhost:9227',
        changeOrigin: true,
      },
    },
  },
  // @ts-expect-error — vitest not yet installed; install vitest + jsdom and remove this directive
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test-setup.ts'],
    include: ['src/**/*.test.{ts,tsx}'],
  },
})
