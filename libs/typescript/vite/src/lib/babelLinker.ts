export { createBabelLinker };

import type { LinkPartialResult, PartialLinker } from './linkPartial';

/**
 * Fallback Angular partial-declaration linker backed by `@angular/compiler-cli`'s official Babel
 * linker plugin.
 *
 * The primary linker is the Rust/NAPI addon (`@treaty/authoring-node`.`linkPartial`). It is fast but
 * currently links only the DI + pipe family (`ɵɵngDeclareFactory`/`Injectable`/`Injector`/`NgModule`/
 * `Pipe`); it leaves `ɵɵngDeclareComponent`/`ɵɵngDeclareDirective` untouched. Any residual
 * `ɵɵngDeclare*` call still triggers the runtime JIT fallback - the exact failure this feature
 * exists to prevent. This Babel backend is the EXACT mechanism the Angular CLI uses to de-partial
 * published libraries and it links the WHOLE `ɵɵngDeclare*` family (components and directives
 * included), so it is the correct backend for the "no JIT / no `@angular/compiler` at runtime"
 * guarantee until the Rust linker covers components/directives too.
 *
 * Why this stays safe:
 *   * The linker runs at BUILD / dev-transform time, in the Node build process - never in the
 *     browser. It reads the `ɵɵngDeclare*` partial declarations and emits their AOT `ɵɵdefine*`
 *     forms inline. `@angular/compiler` / `@angular/compiler-cli` are build-time-only dependencies
 *     of the linker; they are NOT imported by, or bundled into, the linked module output (verified:
 *     the linked `@angular/common` carries no `@angular/compiler` import).
 *   * `linkerJitMode: false` emits ahead-of-time definitions, so the runtime never reaches the JIT
 *     fallback that throws "needs JIT / `@angular/compiler` not available".
 *
 * We use the module's DEFAULT export (`require('@angular/compiler-cli/linker/babel').default`),
 * which is the Babel preset entry that auto-provides the linker's required `fileSystem`
 * (`NodeJSFileSystem`) and `logger` (`ConsoleLogger`) - the bare `createEs2015LinkerPlugin` throws
 * without them. Angular packages are resolved lazily by name so they bind against the consuming
 * app's installed Angular version; any resolution failure degrades to "no fallback available"
 * rather than throwing at plugin construction time.
 */

/** The `@angular/compiler-cli/linker/babel` module shape we depend on (its default Babel plugin). */
interface AngularLinkerBabelModule {
  default: unknown;
}

/** Minimal shape of the `@babel/core` API we depend on. */
interface BabelCoreModule {
  transformSync(
    code: string,
    options: {
      filename: string;
      compact: boolean;
      babelrc: boolean;
      configFile: boolean;
      plugins: unknown[];
      parserOpts: { sourceType: 'module' };
      sourceMaps: boolean;
    },
  ): { code: string | null } | null;
}

/**
 * Build a {@link PartialLinker} that links partial declarations via the Angular Babel linker, or
 * return `null` when the required build-time packages (`@babel/core`,
 * `@angular/compiler-cli/linker/babel`) cannot be resolved.
 */
function createBabelLinker(): PartialLinker | null {
  let babel: BabelCoreModule;
  let linkerBabel: AngularLinkerBabelModule;
  try {
    babel = require('@babel/core') as BabelCoreModule;
    linkerBabel = require('@angular/compiler-cli/linker/babel') as AngularLinkerBabelModule;
  } catch {
    return null;
  }

  if (typeof babel.transformSync !== 'function' || typeof linkerBabel.default !== 'function') {
    return null;
  }

  return {
    linkPartial(code: string, filename: string): LinkPartialResult {
      try {
        const result = babel.transformSync(code, {
          filename,
          compact: false,
          babelrc: false,
          configFile: false,
          // The default export is a Babel preset-style plugin: passing `[plugin, options]` invokes
          // it as `plugin(babelApi, options)`, which constructs the ES2015 linker pre-wired with a
          // Node filesystem + console logger. `linkerJitMode: false` => emit AOT `ɵɵdefine*`.
          plugins: [[linkerBabel.default, { linkerJitMode: false }]],
          parserOpts: { sourceType: 'module' },
          // The linker is a localized AST rewrite; we keep offsets simple and let the bundler own
          // vendor source maps (the Vite plugin returns a null/identity map for these libs).
          sourceMaps: false,
        });
        const linked = result?.code;
        if (typeof linked !== 'string') {
          return {
            code,
            errors: [`Angular Babel linker produced no output for ${filename}`],
          };
        }
        return { code: linked, errors: [] };
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return { code, errors: [message] };
      }
    },
  };
}
