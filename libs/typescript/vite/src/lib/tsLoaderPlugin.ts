export { TsLoaderPlugin };

import type { Plugin } from 'vite';
import { transform } from '@swc/core';

const TS_RE = /\.[cm]?tsx?(\?|$)/;

/**
 * Minimal first-party TypeScript loader: strips types from `.ts`/`.tsx`/`.mts`/`.cts` modules (and
 * their query-suffixed variants) via SWC, leaving JavaScript semantics untouched.
 *
 * Unlike the full `DevelopmentPlugin`, this does NOT inject the `<script src="/@angular/compiler">`
 * JIT runtime into the HTML, and it does NOT run the `@Component`-rewriting SWC visitors. That is
 * deliberate: this loader pairs with the partial-declaration linker for apps whose first-party
 * components are already AOT (a hand-written / pre-compiled `ɵcmp`/`ɵfac`, or Treaty Rust compiler
 * output). Keeping the compiler out of the page is what preserves the "no JIT / no
 * `@angular/compiler`" guarantee end to end.
 *
 * `node_modules` is left to the linker / esbuild prebundle path; this transform is first-party only.
 */
const TsLoaderPlugin: Plugin = {
  name: 'vite-plugin-treaty-ts-loader',
  enforce: 'pre',
  // Disable Vite's built-in esbuild TS transform so types are stripped exactly once, here.
  config() {
    return { esbuild: false };
  },
  async transform(code, id) {
    if (id.includes('node_modules') || !TS_RE.test(id)) {
      return undefined;
    }
    const isTsx = /\.[cm]?tsx(\?|$)/.test(id);
    const result = await transform(code, {
      filename: id,
      sourceMaps: true,
      jsc: {
        target: 'es2022',
        parser: { syntax: 'typescript', tsx: isTsx, decorators: true },
        // Preserve runtime semantics; we only strip TS types here.
        transform: {},
        keepClassNames: true,
      },
      module: { type: 'es6' },
      minify: false,
    });
    return { code: result.code, map: result.map ? JSON.parse(result.map) : null };
  },
};
