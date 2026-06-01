export { LinkPartialConfigPlugin, createLinkPartialPlugins };

import type { Plugin } from 'vite';
import { LinkPartialPlugin, createLinkPartialEsbuildPlugin } from './linkPartialPlugin';
import { DevServePlugin } from './devServePlugin';

/**
 * The Vite `config` half of the Angular partial-declaration linker, isolated from any
 * Angular-CLI / vike concerns.
 *
 * Two things only, both required for the "no JIT / no `@angular/compiler`" guarantee:
 *   * `optimizeDeps.exclude` keeps `@angular/compiler` out of the dependency prebundle (no JIT
 *     runtime ever lands in `node_modules/.vite/deps`); and
 *   * `optimizeDeps.esbuildOptions.plugins` gains {@link createLinkPartialEsbuildPlugin}, which links
 *     partial `@angular` libraries (`ɵɵngDeclare*` → AOT `ɵɵdefine*`) at prebundle time so the linked
 *     output is what reaches the browser.
 *
 * This is the single source of truth for the linker's `config` contribution. The fuller
 * {@link import('./configPlugin').ConfigPlugin} (which also carries vike/SSR/manualChunks wiring for
 * `@treaty/ts-vite`'s `angular()` / `linkAngularPartials()`) reuses this exact plugin rather than
 * re-declaring the optimizeDeps block, and `@treaty/vite` consumes it via
 * {@link createLinkPartialPlugins} without inheriting any Angular-CLI config.
 */
const LinkPartialConfigPlugin: Plugin = {
  name: 'vite-plugin-treaty-link-partial-config',
  enforce: 'pre',
  config() {
    return {
      optimizeDeps: {
        exclude: ['@angular/compiler'],
        esbuildOptions: {
          plugins: [createLinkPartialEsbuildPlugin()],
        },
      },
    };
  },
};

/**
 * Build the complete set of Vite plugins that link published Angular partial-declaration libraries
 * to AOT, so a host plugin can spread them into its own plugin array. This is the reusable linker
 * surface every `@treaty` Vite integration (`@treaty/vite`, `@treaty/ts-vite`) consumes - the linking
 * logic itself lives ONCE in Rust (`@treaty/authoring-node`.`linkPartial`); these plugins are the thin
 * bundler wiring around it.
 *
 * The returned plugins, in pipeline order:
 *   1. {@link LinkPartialConfigPlugin} - excludes `@angular/compiler` from prebundling and links
 *      partial deps at esbuild prebundle time (dev-serve and build alike).
 *   2. {@link LinkPartialPlugin} - the `enforce: 'pre'` `transform` that de-partials non-prebundled
 *      partial `node_modules` modules in the module graph (dev-serve and build alike).
 *   3. {@link DevServePlugin} - the dev-serve `index.html` owner that injects NO
 *      `@angular/compiler` script and defensively strips one if another plugin added it.
 *
 * Spreading these is sufficient for the "no JIT / no `@angular/compiler`" guarantee: a host that
 * serves partial Angular libraries needs nothing more than `[...createLinkPartialPlugins()]` to keep
 * the JIT compiler off both the prebundle and the page.
 */
function createLinkPartialPlugins(): Plugin[] {
  return [LinkPartialConfigPlugin, LinkPartialPlugin, DevServePlugin];
}
