import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react()],
  build: {
    target: "es2024",
    outDir: "dist",
  },
  server: {
    proxy: {
      "/api": {
        target: `http://127.0.0.1:${process.env.MOKMOK_WEB_PORT ?? "8787"}`,
        changeOrigin: true,
        ws: true,
      },
    },
  },
  test: {
    include: ["src/**/*.test.{ts,tsx}"],
    environment: "node",
  },
});
