export { DevelopmentPlugin };

import type { Plugin } from 'vite';
import { swcTransform } from './swc';

const DevelopmentPlugin: Plugin = {
  name: 'vite-plugin-angular-dev',
  enforce: 'pre',
  apply() {
    // Applies to BOTH `serve` AND `build`: the AOT `BuildPlugin` is disabled
    // (commented out in ./index.ts), so this plugin's swc `.ts` transform + the
    // `@angular/compiler` JIT injection are the de-facto compile path for the REPL
    // in both modes. (Gating it to `serve` only — the aspirational AOT split in the
    // old comment — left production `vite build` with no `.ts` transpiler.)
    return true;
  },
  config() {
    return {
      esbuild: false,
    };
  },
  transformIndexHtml(html) {
    const compilerScript = `<script type="module" src="/@angular/compiler"></script>`;
    return html.replace('</head>', `${compilerScript}</head>`);
  },
  resolveId(id) {
    if (id.startsWith('/@angular/compiler')) {
      return this.resolve(id.substring(1));
    }
    return;
  },
  transform(code, id) {
    return swcTransform({
      code,
      id,
    });
  },
};