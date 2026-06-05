export {
  PARTIAL_MARKER,
  isPartialModule,
  loadLinker,
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
 * Which backend handled a given module, for observability.
 *
 * Linking now lives ENTIRELY in Rust: the complete `treaty_ivy::linker::link_partial` (exposed as
 * the NAPI `linkPartial` of `@treaty/authoring-node`) de-partials every `ɵɵngDeclare*` kind
 * (Factory/Injectable/Injector/NgModule/Pipe/Directive/Component/ClassMetadata) to zero residual.
 * There is no longer a Babel finisher on the hot path, so the only backend ever recorded is
 * `'rust'` (a module that links with no diagnostics). The seam is retained for the e2e to assert
 * the Rust addon is the linker for every module it touches.
 */
type LinkBackend = 'rust';

/**
 * Cheap detector for a partial-compiled module that must be linked.
 *
 * A module needs linking only when it both lives under `node_modules` (published, already
 * partial-compiled - first-party sources are full-compiled by the Treaty/Angular pipeline) and
 * actually contains a `ɵɵngDeclare*` call. The `node_modules` guard keeps the (more expensive)
 * substring scan off the hot path for first-party files.
 *
 * Exported as part of the shared linker surface so every `@treaty` bundler plugin (Vite, rsbuild,
 * rspack/module-federation, rslib) reuses the SAME partial detector rather than re-implementing it.
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
 * Populated by {@link loadLinker} and read via {@link getLinkBackend}. This is the observability seam
 * the e2e uses to assert the Rust addon is the linker.
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
 * Lazily resolve the partial-declaration linker, memoising the result (including a `null`
 * "unavailable" outcome, so resolution is attempted at most once).
 *
 * Linking logic lives ONCE, in Rust: this resolves ONLY the Rust/NAPI addon
 * `@treaty/authoring-node`.`linkPartial` - the complete linker that de-partials every
 * `ɵɵngDeclare*` kind to zero residual. We resolve it by package name so this works both inside the
 * monorepo and when `@treaty/ts-vite` is consumed as an installed dependency; a `require`
 * indirection keeps this CommonJS-friendly without pulling the native binary into the module graph
 * at import time.
 *
 * There is no Babel fallback: if the Rust addon (or its `linkPartial` export) is unavailable we
 * degrade to pass-through (`null`) and the caller serves the source unchanged. `@angular/compiler-cli`
 * and `@babel/core` are no longer on the linker hot path at all.
 */
function loadLinker(): PartialLinker | null {
  if (cachedLinker !== undefined) {
    return cachedLinker;
  }

  try {
    const addon = require('@treaty/authoring-node') as Partial<PartialLinker>;
    if (typeof addon.linkPartial === 'function') {
      const linkPartial = addon.linkPartial.bind(addon);
      cachedLinker = {
        linkPartial(code: string, filename: string): LinkPartialResult {
          const result = linkPartial(code, filename);
          if (result.errors.length === 0) {
            backendByModule.set(filename, 'rust');
          }
          return result;
        },
      };
      return cachedLinker;
    }
  } catch {
    // Addon unavailable: degrade to pass-through below.
  }

  cachedLinker = null;
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
 * is not partial-compiled or when the Rust linker addon is unavailable (caller should serve the
 * source unchanged in both cases).
 *
 * Throws when the linker reports diagnostics, so a genuinely broken partial declaration fails the
 * build/serve loudly instead of silently shipping un-linked (JIT-dependent) output.
 *
 * This is the single shared linker entry every `@treaty` bundler plugin imports - the bundler-side
 * code is a thin shim (detect-partial + content-cache + call Rust); the linking itself is Rust-only.
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
