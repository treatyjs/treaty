export { DevServePlugin, COMPILER_SCRIPT_RE };

import type { Plugin } from 'vite';

/**
 * Matches an injected `<script ... src="[/]@angular/compiler">` tag (any attribute order / quoting),
 * i.e. the JIT-runtime injection the full {@link import('./devPlugin').DevelopmentPlugin} adds. Used
 * to assert-and-strip: under {@link import('./index').linkAngularPartials} no such script must ever
 * reach the page.
 */
const COMPILER_SCRIPT_RE =
  /<script\b[^>]*\bsrc\s*=\s*["']\/?@angular\/compiler["'][^>]*>\s*<\/script>\s*/gi;

/**
 * The lean dev-serve owner of `index.html` for {@link import('./index').linkAngularPartials}.
 *
 * `linkAngularPartials()` is the "no JIT / no `@angular/compiler`" entry: partial `node_modules`
 * Angular libraries are de-partialled (`ɵɵngDeclare*` → AOT `ɵɵdefine*`) at prebundle/transform time
 * (the esbuild `optimizeDeps` plugin from {@link import('./configPlugin').ConfigPlugin} and the
 * {@link import('./linkPartialPlugin').LinkPartialPlugin} transform - both active under `vite dev`),
 * and first-party `.ts` is type-stripped by {@link import('./tsLoaderPlugin').TsLoaderPlugin}. Because
 * AOT definitions are present at runtime, the JIT compiler is never reached - so the page must NOT
 * load `@angular/compiler`.
 *
 * The full {@link import('./devPlugin').DevelopmentPlugin} injects
 * `<script src="/@angular/compiler">` into the HTML (the JIT path); that plugin is deliberately NOT
 * part of `linkAngularPartials()`. This plugin replaces it with a minimal, JIT-free index transform
 * that does nothing JIT-related: it owns the dev-serve HTML and, defensively, strips any
 * `@angular/compiler` script another plugin in the chain might have injected, so the guarantee holds
 * end to end regardless of ordering. It performs no other HTML rewriting.
 *
 * It is dev-serve only (`apply: 'serve'`): the production `vite build` emits `index.html` without any
 * such injection already, so there is nothing to own there.
 */
const DevServePlugin: Plugin = {
  name: 'vite-plugin-treaty-dev-serve',
  apply: 'serve',
  // Run last among index-html transforms so a strip here wins over any earlier injector.
  transformIndexHtml: {
    order: 'post',
    handler(html: string): string {
      // No JIT runtime is injected here. Defensively remove any `@angular/compiler` script so the
      // page never reaches the JIT compiler in dev (the AOT-linked libraries make it unnecessary).
      return html.replace(COMPILER_SCRIPT_RE, '');
    },
  },
};
