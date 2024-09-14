/// <reference types='vitest' />
import { defineConfig, sortUserPlugins } from 'vite';

import { nxViteTsPaths } from '@nx/vite/plugins/nx-tsconfig-paths.plugin';

import { treatySFC } from './src/tools/treaty-sfc/compiler'
import { angular } from './src/tools/angular'


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

  assetsInclude: ["**/*.grammar"],



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
