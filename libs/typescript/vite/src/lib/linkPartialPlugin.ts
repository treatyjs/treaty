export { LinkPartialPlugin, createLinkPartialEsbuildPlugin };

import { readFile } from 'fs/promises';
import type { DepOptimizationConfig, Plugin } from 'vite';
import { linkPartialCode } from './linkPartial';

/**
 * The Angular partial-declaration linker, wired as a Vite plugin.
 *
 * Published Angular libraries (`node_modules/@angular/* /fesm2022/*.mjs`) ship *partial*-compiled:
 * every decorated class emits a `ɵɵngDeclare*({...})` call. Left un-linked, Angular falls back to
 * the JIT compiler at runtime and throws "needs JIT / `@angular/compiler` not available" the moment
 * `@angular/compiler` is excluded (which it is, both in dev and prod). This plugin rewrites those
 * declarations to their AOT `ɵɵdefine*` form via the Rust/NAPI linker so NO JIT and NO
 * `@angular/compiler` are needed.
 *
 * It runs on BOTH paths:
 *   * DEV — the `transform` hook below catches partial `node_modules` modules served on the fly
 *     (those excluded from esbuild prebundling), and {@link createLinkPartialEsbuildPlugin} (added
 *     to `optimizeDeps.esbuildOptions.plugins`) links those that ARE prebundled, at esbuild load
 *     time, so the linked output reaches the prebundle cache (`node_modules/.vite/deps`).
 *   * BUILD — the same `transform` hook runs over the rollup module graph, linking partial vendored
 *     chunks before they are bundled.
 *
 * `enforce: 'pre'` ensures we de-partial the source before any downstream JS transform inspects it.
 * Linking preserves byte offsets outside the rewritten spans, so for these vendored libraries we
 * return a `null` (identity) source map rather than fabricating one.
 */
const LinkPartialPlugin: Plugin = {
  name: 'vite-plugin-treaty-link-partial',
  enforce: 'pre',
  apply() {
    // Partial libraries must be linked in dev (serve) AND prod (build).
    return true;
  },
  transform(code, id) {
    const linked = linkPartialCode(code, id);
    if (linked === null) {
      return undefined;
    }
    // Identity map: the linker is a span rewrite that preserves offsets outside the rewritten
    // `ɵɵngDeclare*` calls; a passthrough/null map is acceptable for vendored libraries.
    return { code: linked, map: null };
  },
};

type EsbuildOptions = NonNullable<DepOptimizationConfig['esbuildOptions']>;
type EsbuildPlugin = NonNullable<EsbuildOptions['plugins']>[number];

/**
 * An esbuild plugin (for `optimizeDeps.esbuildOptions.plugins`) that links Angular partial
 * declarations at dependency-prebundle time.
 *
 * Vite prebundles bare imports with esbuild into `node_modules/.vite/deps`; the Vite `transform`
 * hook above does NOT see those prebundled modules. Without this, partial `@angular` libraries would
 * be served raw from the prebundle cache and trigger the JIT fallback. By hooking esbuild's `onLoad`
 * for `.[cm]?js` files under `node_modules`, we link the source before esbuild bundles it, so the
 * linked (AOT) output is what lands in the prebundle cache and reaches the browser.
 */
function createLinkPartialEsbuildPlugin(): EsbuildPlugin {
  return {
    name: 'treaty-link-partial-deps',
    setup(build) {
      build.onLoad({ filter: /\.[cm]?js$/, namespace: 'file' }, async args => {
        if (!args.path.includes('node_modules')) {
          return null;
        }
        const source = await readFile(args.path, 'utf-8');
        const linked = linkPartialCode(source, args.path);
        if (linked === null) {
          return null;
        }
        return { contents: linked, loader: 'js' };
      });
    },
  };
}
