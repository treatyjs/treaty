/// <reference types='vitest' />
import { defineConfig } from 'vite';

import { nxViteTsPaths } from '@nx/vite/plugins/nx-tsconfig-paths.plugin';
import { treatySFC } from '@treaty/ts-compiler'
import { angular } from '@treaty/ts-vite'


export default defineConfig({
  root: __dirname,
  cacheDir: '../../node_modules/.vite/apps/repl',

  server: {
    port: 4200,
    host: 'localhost',
  },

  preview: {
    port: 4300,
    host: 'localhost',
  },

  plugins: [
    nxViteTsPaths(),
    treatySFC(),
    angular(),
  ],

  build: {
    outDir: '../../dist/apps/repl',
    reportCompressedSize: true,
    commonjsOptions: {
      transformMixedEsModules: true,
    },
  },

  test: {
    globals: true,
    cache: {
      dir: '../../node_modules/.vitest',
    },
    environment: 'jsdom',
    include: ['src/**/*.{test,spec}.{js,mjs,cjs,ts,mts,cts,jsx,tsx,treaty}'],

    reporters: ['default'],
    coverage: {
      reportsDirectory: '../../coverage/apps/repl',
      provider: 'v8',
    },
  },
});
