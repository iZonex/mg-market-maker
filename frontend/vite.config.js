import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

export default defineConfig({
  plugins: [svelte()],
  server: {
    port: 3000,
    proxy: {
      '/api': 'http://localhost:9090',
      '/ws': {
        target: 'ws://localhost:9090',
        ws: true,
      },
      '/health': 'http://localhost:9090',
      '/metrics': 'http://localhost:9090',
    },
  },
  build: {
    outDir: 'dist',
  },
})
