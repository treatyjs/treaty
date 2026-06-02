/// <reference types='vitest' />
import { join } from 'node:path';
import { defineConfig } from 'vite';

import { treatySFC } from './src/tools/treaty-sfc/compiler'
import { angular } from './src/tools/angular'
import { treatyGallery } from './src/tools/gallery-plugin'


export default defineConfig({
  base: './',
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
    treatyGallery(join(__dirname, 'src/gallery/samples')),
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
