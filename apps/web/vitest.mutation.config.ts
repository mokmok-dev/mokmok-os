import { defineConfig, mergeConfig } from 'vitest/config';

import base from './vite.config';

// The app flow is excluded from the mutation gate: it covers the components, but
// replaying it for every mutant made the gate an order of magnitude slower without
// killing a different mutant set.
export default mergeConfig(
  base,
  defineConfig({
    test: {
      exclude: ['src/tests/app.test.tsx'],
    },
  }),
);
