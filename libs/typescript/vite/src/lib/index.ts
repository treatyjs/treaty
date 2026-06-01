export { angular, linkAngularPartials };

import type { Plugin } from 'vite';
import { DirImporterPlugin } from './dirImporterPlugin';
import { ConfigPlugin } from './configPlugin';
import { DevelopmentPlugin } from './devPlugin';
import { LinkPartialPlugin } from './linkPartialPlugin';
import { TsLoaderPlugin } from './tsLoaderPlugin';

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
 *     partial-declaration linker into the dependency-prebundle (esbuild) step.
 *   * {@link LinkPartialPlugin} links partial Angular libraries in the rollup module graph
 *     (`ɵɵngDeclare*` → AOT `ɵɵdefine*`) for dev-serve and prod-build alike.
 *   * {@link TsLoaderPlugin} strips types from first-party `.ts`/`.tsx` so the app's own modules
 *     load, WITHOUT injecting the `@angular/compiler` JIT script that the full dev plugin adds.
 *
 * This is the right entry for apps that ship AOT-authored first-party components (a hand-written or
 * pre-compiled `ɵcmp`/`ɵfac`, or the Treaty Rust component compiler output) and only need the
 * published partial Angular libraries de-partialled. It does NOT pull in the Angular-CLI
 * (`@angular-devkit/build-angular`) build pipeline.
 */
function linkAngularPartials(): Plugin[] {
  return [...ConfigPlugin, LinkPartialPlugin, DirImporterPlugin, TsLoaderPlugin];
}
