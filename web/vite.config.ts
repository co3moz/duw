import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  // Relative asset URLs so the embedded bundle works regardless of mount point.
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    // The report is a single view; one chunk keeps the embedded payload small.
    chunkSizeWarningLimit: 900,
  },
  server: {
    port: 5173,
    // For UI work: run `duw --port 8080 --no-open <dir>` alongside `npm run dev`.
    proxy: {
      '/api': { target: 'http://127.0.0.1:8080', changeOrigin: true },
    },
  },
})
