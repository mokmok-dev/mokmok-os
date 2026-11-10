import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';

const controlPlane = process.env.MOKMOK_CONTROL_URL ?? 'http://127.0.0.1:8787';
const proxy = {
  '/api': { target: controlPlane, changeOrigin: true },
};

export default defineConfig({
  plugins: [react()],
  server: { proxy },
  preview: { proxy },
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/tests/setup.ts'],
    include: ['src/**/*.test.{ts,tsx}'],
    coverage: { enabled: false },
    restoreMocks: true,
  },
});
