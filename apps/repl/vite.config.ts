/// <reference types='vitest' />
import { join } from 'node:path';
import { defineConfig } from 'vite';

import { angular } from './src/tools/angular'
import { treatyGallery } from './src/tools/gallery-plugin'
// The REAL Treaty bundler plugin — the SAME oxc-based integration consumed by
// @treaty/rspack/@treaty/rsbuild etc. It lowers EVERY Treaty/Angular authoring
// surface to Ivy via the Rust addon (compile of .treaty/.tsx/normal Angular .ts of
// every decorator kind) AND links published @angular/* partials to AOT in Rust,
// excluding @angular/compiler — no JIT. The REPL uses our compiler, and ONLY ours.
import treaty from '@treaty/vite'


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
    // THE Treaty compiler is the sole compile path (oxc addon: compile + linkPartial).
    ...treaty(),
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
