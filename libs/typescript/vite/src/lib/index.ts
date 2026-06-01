export {
  angular,
  linkAngularPartials,
  createLinkPartialPlugins,
  getLinkBackend,
  isPartialModule,
  linkPartialCode,
  type LinkBackend,
};

import type { Plugin } from 'vite';
import { DirImporterPlugin } from './dirImporterPlugin';
import { ConfigPlugin } from './configPlugin';
import { DevelopmentPlugin } from './devPlugin';
import { DevServePlugin } from './devServePlugin';
import { LinkPartialPlugin } from './linkPartialPlugin';
import { createLinkPartialPlugins } from './linkPartialPlugins';
import { TsLoaderPlugin } from './tsLoaderPlugin';
import {
  getLinkBackend,
  isPartialModule,
  linkPartialCode,
  type LinkBackend,
} from './linkPartial';

/**
 * The full `@treaty/ts-vite` Angular plugin set.
 *
 * Includes the legacy Angular-CLI-backed build/optimizer plugins (`BuildPlugin`), which depend on
 * `@angular-devkit/build-angular` internals. Those internals moved between Angular major versions,
 * so `BuildPlugin` is required LAZILY here: importing `@treaty/ts-vite` (e.g. to use only the
 * partial-declaration linker via {@link linkAngularPartials}) never loads those deep devkit paths,
 * and a devkit version mismatch cannot crash module load.
 */
function angular(): Plugin[] {
  // Lazy require: keep the devkit-coupled BuildPlugin off the module-load path (see above).

  const { BuildPlugin } = require('./buildPlugin') as typeof import('./buildPlugin');
  return [
    ...ConfigPlugin,
    // Link published Angular partial-declaration libs (`ɵɵngDeclare*` → AOT `ɵɵdefine*`) before
    // any other JS transform sees them, in dev (serve) and prod (build) alike. `enforce: 'pre'`
    // keeps it ahead of the dev/build transforms below.
    LinkPartialPlugin,
    DirImporterPlugin,
    DevelopmentPlugin,
    ...BuildPlugin(),
  ];
}

/**
 * A lean plugin set whose job is exactly the "no JIT / no `@angular/compiler`" guarantee:
 *
 *   * {@link ConfigPlugin} excludes `@angular/compiler` from optimizeDeps and wires the
 *     partial-declaration linker into the dependency-prebundle (esbuild) step - active under both
 *     `vite dev`/serve and `vite build`, so partial `node_modules` deps are de-partialled on the fly
 *     when the dev server prebundles them.
 *   * {@link LinkPartialPlugin} links partial Angular libraries in the module graph
 *     (`ɵɵngDeclare*` → AOT `ɵɵdefine*`) for dev-serve and prod-build alike (its `transform` hook
 *     catches non-prebundled partial `node_modules` modules served on the fly in dev).
 *   * {@link TsLoaderPlugin} strips types from first-party `.ts`/`.tsx` so the app's own modules
 *     load, WITHOUT injecting the `@angular/compiler` JIT script that the full dev plugin adds.
 *   * {@link DevServePlugin} owns the dev-serve `index.html` transform: it injects NO
 *     `<script src=".../@angular/compiler">`, so the JIT compiler never reaches the page in dev
 *     either. (The full {@link DevelopmentPlugin}, used only by {@link angular}, is the JIT path and
 *     is deliberately excluded here.)
 *
 * This is the right entry for apps that ship AOT-authored first-party components (a hand-written or
 * pre-compiled `ɵcmp`/`ɵfac`, or the Treaty Rust component compiler output) and only need the
 * published partial Angular libraries de-partialled. It does NOT pull in the Angular-CLI
 * (`@angular-devkit/build-angular`) build pipeline, and it never injects the `@angular/compiler` JIT
 * runtime in dev OR prod - the whole point is "no JIT, no `@angular/compiler`" end to end.
 */
function linkAngularPartials(): Plugin[] {
  return [
    ...ConfigPlugin,
    LinkPartialPlugin,
    DirImporterPlugin,
    TsLoaderPlugin,
    DevServePlugin,
  ];
}
