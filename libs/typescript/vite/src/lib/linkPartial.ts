export {
  PARTIAL_MARKER,
  isPartialModule,
  loadLinker,
  composeLinkers,
  linkPartialCode,
  resetLinkerForTesting,
  getLinkBackend,
  resetLinkBackendForTesting,
  type LinkBackend,
  type LinkPartialResult,
  type PartialLinker,
};

/**
 * Angular partial-compilation marker.
 *
 * Published Angular libraries (e.g. `node_modules/@angular/<pkg>/fesm2022/*.mjs`) ship
 * *partial-compiled* output: every `@Component`/`@Directive`/`@Pipe`/`@Injectable`/`@NgModule`
 * is emitted as a `ɵɵngDeclare*` call (`ɵɵngDeclareComponent`, `ɵɵngDeclareDirective`,
 * `ɵɵngDeclareFactory`, ...). Until those declarations are *linked* into their AOT
 * `ɵɵdefine*` form, Angular falls back to the JIT compiler at runtime - which throws
 * "needs JIT / `@angular/compiler` not available" once `@angular/compiler` is excluded.
 *
 * The whole `ɵɵngDeclare*` family shares this exact prefix, so a substring test against the
 * module source is a sufficient (and cheap) detector.
 */
import { createBabelLinker } from './babelLinker';

const PARTIAL_MARKER = 'ɵɵngDeclare';

/**
 * Result of linking one partial-compiled module - mirrors the addon's `linkPartial` return (the
 * NAPI `CompiledComponent` shape: `{ code, errors, map? }`). Linking is a span rewrite of an
 * existing module, so `map` is always absent here and `code` is byte-identical to the input outside
 * the rewritten `ɵɵngDeclare*` call spans.
 */
interface LinkPartialResult {
  /** The linked JavaScript (`ɵɵngDeclare*` rewritten to AOT `ɵɵdefine*`). */
  code: string;
  /** Linker diagnostics (empty on success). */
  errors: string[];
  /** Always absent for linking; declared for shape-compatibility with the addon's return type. */
  map?: string;
}

/** The single addon entry this module depends on. */
interface PartialLinker {
  linkPartial(code: string, filename: string): LinkPartialResult;
}

/**
 * Which backend handled a given module, for observability (the e2e asserts the Rust addon is the
 * primary backend that owns every module it can fully link, and that Babel handles only the modules
 * the Rust linker cannot yet fully link):
 *
 *   * `'rust'`  - the Rust/NAPI addon (the PRIMARY backend) linked the module completely (no residual
 *                 `ɵɵngDeclare*`, no error); Babel did NOT run. As the Rust linker grows to cover
 *                 every declaration kind, more modules become `'rust'` and `@angular/compiler-cli`
 *                 drops off the hot path entirely.
 *   * `'babel'` - the Rust addon was tried first but could not fully link this module (it left a
 *                 residual `ɵɵngDeclare*` it does not yet cover, reported an error, or its
 *                 `linkPartial` export was unavailable), so the complete Babel linker linked the
 *                 module's original source.
 */
type LinkBackend = 'rust' | 'babel';

/**
 * Cheap detector for a partial-compiled module that must be linked.
 *
 * A module needs linking only when it both lives under `node_modules` (published, already
 * partial-compiled - first-party sources are full-compiled by the Treaty/Angular pipeline) and
 * actually contains a `ɵɵngDeclare*` call. The `node_modules` guard keeps the (more expensive)
 * substring scan off the hot path for first-party files.
 */
function isPartialModule(id: string, code: string): boolean {
  if (!id.includes('node_modules')) {
    return false;
  }
  return code.includes(PARTIAL_MARKER);
}

let cachedLinker: PartialLinker | null | undefined;

/**
 * Per-module record of which backend handled the most recent link of each file, keyed by module id.
 * Populated by {@link composeLinkers}/{@link loadLinker} and read via {@link getLinkBackend}. This is
 * the observability seam the e2e uses to assert "Rust primary, Babel only for residual".
 */
const backendByModule = new Map<string, LinkBackend>();

/** Report which backend last linked `id`, or `undefined` if it was never linked. */
function getLinkBackend(id: string): LinkBackend | undefined {
  return backendByModule.get(id);
}

/** Test seam: clear the recorded per-module backends. */
function resetLinkBackendForTesting(): void {
  backendByModule.clear();
}

/**
 * Compose the (fast) Rust addon linker as the PRIMARY backend with the (complete) Babel linker as a
 * residual-only finisher, so the output is guaranteed free of residual `ɵɵngDeclare*` calls
 * regardless of which declaration kinds the Rust linker covers today.
 *
 * The Rust addon is always run FIRST and OWNS every module it can fully link. The Babel linker
 * (`@angular/compiler-cli`) is consulted ONLY to finish what the Rust linker cannot yet complete:
 *
 *   * Rust fully links the module (no residual `ɵɵngDeclare*`, no error) ⇒ ship the Rust output as-is
 *     ⇒ `'rust'`. Babel is NOT invoked.
 *   * Rust leaves a residual `ɵɵngDeclare*` (today: `ɵɵngDeclareComponent`/`Directive`, which it does
 *     not yet cover) or reports an error (a declaration kind it has not learned yet, e.g. an injector
 *     with `providers`). In either case the module is not fully Rust-linkable today, so the COMPLETE
 *     reference Babel linker links the ORIGINAL source for that module ⇒ `'babel'`.
 *
 * Why Babel re-links the ORIGINAL source rather than Rust's partial output: the Ivy definitions of a
 * single class are interdependent (the `ɵfac` factory shape is coupled to its `ɵcmp`/`ɵdir`/`ɵprov`).
 * Rust rewriting a class's factory while Babel rewrites that same class's directive over Rust's output
 * yields an inconsistent definition (it manifests at runtime as "constructor was not compatible with
 * Dependency Injection"). Handing Babel the original source produces one self-consistent linked module.
 * A module Rust links in FULL is internally consistent and ships untouched - so as the Rust linker
 * grows to cover components/directives, more modules become `'rust'` and `@angular/compiler-cli`
 * leaves the hot path entirely. The chosen backend is recorded per module for the e2e to assert.
 */
function composeLinkers(addon: PartialLinker, babel: PartialLinker | null): PartialLinker {
  if (babel === null) {
    return {
      linkPartial(code: string, filename: string): LinkPartialResult {
        const result = addon.linkPartial(code, filename);
        if (result.errors.length === 0) {
          backendByModule.set(filename, 'rust');
        }
        return result;
      },
    };
  }
  return {
    linkPartial(code: string, filename: string): LinkPartialResult {
      // Rust addon is the primary backend - always first.
      const first = addon.linkPartial(code, filename);
      if (first.errors.length === 0 && !first.code.includes(PARTIAL_MARKER)) {
        // Rust fully linked the module on its own (no residual, no error) - ship it, no Babel.
        backendByModule.set(filename, 'rust');
        return first;
      }
      // Rust could not fully link this module (residual `ɵɵngDeclare*` it does not yet cover, or an
      // error on a declaration kind it has not learned). Hand the ORIGINAL source to the complete
      // Babel linker so the whole module is linked once, self-consistently.
      const finished = babel.linkPartial(code, filename);
      if (finished.errors.length === 0) {
        backendByModule.set(filename, 'babel');
      }
      return finished;
    },
  };
}

/**
 * Lazily resolve a partial-declaration linker, memoising the result (including a `null`
 * "unavailable" outcome, so resolution is attempted at most once).
 *
 * Backends, in order of preference:
 *   1. The Rust/NAPI addon `@treaty/authoring-node`.`linkPartial` (primary - fastest) when its
 *      `linkPartial` export is present. We resolve it by package name so this works both inside the
 *      monorepo and when `@treaty/ts-vite` is consumed as an installed dependency; a `require`
 *      indirection keeps this CommonJS-friendly without pulling the native binary into the module
 *      graph at import time. Because the addon does not yet link components/directives, it is
 *      composed (see {@link composeLinkers}) with the Babel linker to clear any residual partials.
 *   2. The Angular Babel linker (`@angular/compiler-cli/linker/babel` via `@babel/core`) - the exact
 *      mechanism the Angular CLI uses to de-partial libraries, and complete across every
 *      `ɵɵngDeclare*` kind. Used directly when the addon's `linkPartial` is absent (e.g. an older
 *      prebuilt addon). It runs at build/dev-transform time only and does NOT bundle
 *      `@angular/compiler` into the app output (see {@link createBabelLinker}).
 *
 * If neither backend is available we degrade to pass-through (`null`).
 */
function loadLinker(): PartialLinker | null {
  if (cachedLinker !== undefined) {
    return cachedLinker;
  }

  const babel = createBabelLinker();

  try {
    const addon = require('@treaty/authoring-node') as Partial<PartialLinker>;
    if (typeof addon.linkPartial === 'function') {
      // Rust addon present: it is the PRIMARY backend, composed with Babel only to finish residual
      // declarations it does not yet cover (see {@link composeLinkers}).
      cachedLinker = composeLinkers(
        { linkPartial: addon.linkPartial.bind(addon) },
        babel,
      );
      return cachedLinker;
    }
  } catch {
    // Fall through to the Babel-only backend.
  }

  // No Rust addon `linkPartial` export: degrade to the Babel-only backend (records `'babel'`).
  if (babel === null) {
    cachedLinker = null;
    return cachedLinker;
  }
  cachedLinker = {
    linkPartial(code: string, filename: string): LinkPartialResult {
      const result = babel.linkPartial(code, filename);
      if (result.errors.length === 0) {
        backendByModule.set(filename, 'babel');
      }
      return result;
    },
  };
  return cachedLinker;
}

/** Test seam: override (or clear) the memoised linker. */
function resetLinkerForTesting(linker?: PartialLinker | null): void {
  cachedLinker = linker;
}

const linkCache = new Map<string, string>();

/**
 * Link one partial-compiled module's source to AOT, caching by content so each distinct module
 * body is linked exactly once across dev requests and the build. Returns `null` when the module
 * is not partial-compiled or when no linker addon is available (caller should serve the source
 * unchanged in both cases).
 *
 * Throws when the linker reports diagnostics, so a genuinely broken partial declaration fails the
 * build/serve loudly instead of silently shipping un-linked (JIT-dependent) output.
 */
function linkPartialCode(code: string, id: string): string | null {
  if (!isPartialModule(id, code)) {
    return null;
  }

  const cached = linkCache.get(code);
  if (cached !== undefined) {
    return cached;
  }

  const linker = loadLinker();
  if (linker === null) {
    return null;
  }

  const result = linker.linkPartial(code, id);
  if (result.errors.length > 0) {
    throw new Error(
      `[treaty] failed to link partial Angular module ${id}:\n${result.errors.join('\n')}`,
    );
  }

  linkCache.set(code, result.code);
  return result.code;
}
