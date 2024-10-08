export { DevelopmentPlugin };

import type { Plugin } from 'vite';
import { swcTransform } from './swc';

const DevelopmentPlugin: Plugin = {
  name: 'vite-plugin-angular-dev',
  enforce: 'pre',
  apply(_, env) {
    // return env.command === 'serve';
    return true
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