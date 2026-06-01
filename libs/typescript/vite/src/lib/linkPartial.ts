export {
  PARTIAL_MARKER,
  isPartialModule,
  loadLinker,
  composeLinkers,
  linkPartialCode,
  resetLinkerForTesting,
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
 * Compose the (fast, partial-coverage) Rust addon linker with the (complete) Babel linker so the
 * output is guaranteed free of residual `ɵɵngDeclare*` calls regardless of which backend is present.
 *
 * The Rust addon currently links only the DI + pipe family and leaves
 * `ɵɵngDeclareComponent`/`Directive` untouched; any residual `ɵɵngDeclare*` would still trigger the
 * runtime JIT fallback. So when the addon output still contains the marker AND a Babel backend is
 * available, we run the Babel linker over the addon's output to finish the component/directive
 * declarations. (The Babel linker is a no-op for already-linked `ɵɵdefine*` calls.)
 */
function composeLinkers(addon: PartialLinker, babel: PartialLinker | null): PartialLinker {
  if (babel === null) {
    return addon;
  }
  return {
    linkPartial(code: string, filename: string): LinkPartialResult {
      const first = addon.linkPartial(code, filename);
      if (first.errors.length > 0 || !first.code.includes(PARTIAL_MARKER)) {
        return first;
      }
      // Residual partial declarations remain (components/directives the addon does not yet link):
      // finish them with the complete Babel linker.
      return babel.linkPartial(first.code, filename);
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
      cachedLinker = composeLinkers(
        { linkPartial: addon.linkPartial.bind(addon) },
        babel,
      );
      return cachedLinker;
    }
  } catch {
    // Fall through to the Babel-only backend.
  }

  cachedLinker = babel;
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
