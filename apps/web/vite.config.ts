import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"

const server = "http://127.0.0.1:8080"

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      "/api": server,
      "/ws": { target: server, ws: true },
    },
  },
})
