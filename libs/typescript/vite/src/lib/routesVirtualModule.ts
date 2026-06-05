export {
  TREATY_ROUTES_ID,
  RESOLVED_TREATY_ROUTES_ID,
  isTreatyRoutesId,
  loadRouteGenerator,
  generateRoutesModule,
  resetRouteGeneratorForTesting,
  type RoutesVirtualModuleOptions,
  type GeneratedRoutesModule,
  type RouteGenerator,
};

import { isAbsolute, resolve } from 'node:path';

/**
 * The bare virtual module specifier a Treaty app imports to obtain its file-system
 * route graph (`import routes from 'virtual:treaty-routes'`). It is never a real
 * file on disk — the bundler plugins below resolve it to {@link RESOLVED_TREATY_ROUTES_ID}
 * and serve {@link generateRoutesModule}'s emitted TypeScript for it.
 *
 * File routing is produced DURING the build, not checked in: there is no prebuilt
 * `routes.ts`. The plugin that owns this id calls the Rust core
 * (`@treaty/authoring-node`.`generateRoutes`, the bundler shim over the pure
 * `treaty_file_routing` crate) on every load, so the route graph always reflects
 * the on-disk `routes/` tree.
 */
const TREATY_ROUTES_ID = 'virtual:treaty-routes';

/**
 * The resolved id every bundler maps {@link TREATY_ROUTES_ID} to. The leading NUL
 * (`\0`) is the Rollup/Vite convention marking a virtual module so no other plugin
 * (or the filesystem) tries to load it; rspack/rsbuild treat it as an opaque
 * resolved request handled by the same `load` callback. Keeping ONE resolved id
 * across bundlers means the load logic is identical everywhere — only the
 * per-bundler registration differs (exactly like the partial-declaration linker).
 */
const RESOLVED_TREATY_ROUTES_ID = '\0virtual:treaty-routes';

/**
 * Whether `id` refers to the Treaty routes virtual module, in either its bare
 * ({@link TREATY_ROUTES_ID}) or resolved ({@link RESOLVED_TREATY_ROUTES_ID}) form.
 * A bundler-appended query/hash suffix (`?foo`, `#bar`) is ignored so an imported
 * `virtual:treaty-routes?used` still matches.
 */
function isTreatyRoutesId(id: string): boolean {
  const clean = id.replace(/[?#].*$/, '');
  return clean === TREATY_ROUTES_ID || clean === RESOLVED_TREATY_ROUTES_ID;
}

/**
 * The knobs a bundler plugin forwards to the route generator, sourced from the
 * Treaty plugin options. Everything except {@link routesRoot} is optional and
 * falls back to the file-routing core defaults.
 */
interface RoutesVirtualModuleOptions {
  /**
   * Absolute (or build-root-relative) path to the project root that CONTAINS the
   * `routes/` and `api/` directories the route graph is generated from. A relative
   * path is resolved against {@link RoutesVirtualModuleOptions.cwd} (defaulting to
   * `process.cwd()`).
   */
  readonly routesRoot: string;
  /**
   * Base directory used to resolve a relative {@link RoutesVirtualModuleOptions.routesRoot}.
   * Defaults to `process.cwd()`. The bundler plugins pass their resolved build root
   * here so the route graph is stable regardless of where the dev/build process was
   * launched.
   */
  readonly cwd?: string;
  /** `routes/` directory name relative to the root (file-routing default when omitted). */
  readonly routesDir?: string;
  /** `api/` directory name relative to the root (file-routing default when omitted). */
  readonly apiDir?: string;
  /**
   * Dynamic-segment authoring style: `'bracket'` (`[id]`) or `'colon'` (`:id`).
   * Defaults to the file-routing core default (`'bracket'`).
   */
  readonly dynamicSegmentStyle?: 'bracket' | 'colon';
  /**
   * Whether to emit Module-Federation remotes for lazy route boundaries. Defaults
   * to the file-routing core default. Set `false` to emit an empty remotes list.
   */
  readonly federation?: boolean;
  /**
   * The `import(...)` path prefix the emitted loaders prepend to each tree-relative
   * route entry. Defaults to the core's `../../`. Because the routes module is
   * served as a virtual module (not written to disk), this governs how a lazy
   * `import('<base>/routes/index.treaty')` loader resolves the real route file.
   */
  readonly importBase?: string;
}

/** The emitted routes module plus the route entry files it references (watch deps). */
interface GeneratedRoutesModule {
  /**
   * The emitted routes module (`export const routes`, `export default routes`,
   * `export const federationRemotes`), down-levelled to plain JS by {@link toJsModule}
   * so every bundler can parse the virtual module directly (a virtual module has no
   * on-disk path and so bypasses a bundler's built-in TS transform). The route graph
   * is byte-identical to the `treaty-file-routing --emit ts` CLI output — only the TS
   * type surface (the `import type`, the `: Routes` annotation, the trailing
   * `as const`) is stripped.
   */
  code: string;
  /**
   * Tree-relative route entry files the module's lazy `import(...)` loaders
   * reference (no import base), de-duplicated. A bundler registers these as watch
   * dependencies so editing/adding/removing a route re-runs the virtual module.
   */
  files: string[];
  /**
   * The same route entry files as {@link GeneratedRoutesModule.files}, but resolved
   * to ABSOLUTE paths against the routes root. These are what a bundler registers
   * with `this.addWatchFile(...)` (Vite/Rollup) or its watch API so a change to any
   * referenced route invalidates the virtual module. Also useful as the watch dir
   * seed so route ADDS/REMOVES (a file not yet in this list) re-trigger generation.
   */
  watchFiles: string[];
}

/** The single addon entry this module depends on (the Rust file-routing core). */
interface RouteGenerator {
  generateRoutes(rootDir: string, configJson: string): GeneratedRoutesModule;
}

let cachedGenerator: RouteGenerator | null | undefined;

/**
 * Lazily resolve the Rust file-routing generator, memoising the result (including a
 * `null` "unavailable" outcome, so resolution is attempted at most once).
 *
 * Routing logic lives ONCE, in Rust: this resolves ONLY the NAPI addon
 * `@treaty/authoring-node`.`generateRoutes` — the bundler shim over the pure
 * `treaty_file_routing` crate. Resolved by package name so it works inside the
 * monorepo and when consumed as an installed dependency; the `require` indirection
 * keeps this CommonJS-friendly without pulling the native binary into the module
 * graph at import time.
 *
 * Returns `null` when the addon (or its `generateRoutes` export) is unavailable, so
 * the caller can surface a clear error rather than crashing on module load.
 */
function loadRouteGenerator(): RouteGenerator | null {
  if (cachedGenerator !== undefined) {
    return cachedGenerator;
  }

  try {
    const addon = require('@treaty/authoring-node') as Partial<RouteGenerator>;
    if (typeof addon.generateRoutes === 'function') {
      const generateRoutes = addon.generateRoutes.bind(addon);
      cachedGenerator = { generateRoutes };
      return cachedGenerator;
    }
  } catch {
    // Addon unavailable: report null below.
  }

  cachedGenerator = null;
  return cachedGenerator;
}

/** Test seam: override (or clear) the memoised route generator. */
function resetRouteGeneratorForTesting(generator?: RouteGenerator | null): void {
  cachedGenerator = generator;
}

/**
 * Translate the bundler-facing {@link RoutesVirtualModuleOptions} into the flat
 * `config_json` the addon's `generateRoutes` accepts (the
 * `PartialFileRoutingConfig` routing fields plus the emit-only `importBase`). Only
 * the fields the caller actually set are serialised, so absent knobs fall through
 * to the file-routing core defaults rather than being pinned here.
 */
function toConfigJson(options: RoutesVirtualModuleOptions): string {
  const config: Record<string, unknown> = {};
  if (options.routesDir !== undefined) config['routesDir'] = options.routesDir;
  if (options.apiDir !== undefined) config['apiDir'] = options.apiDir;
  if (options.dynamicSegmentStyle !== undefined) {
    config['dynamicSegmentStyle'] = options.dynamicSegmentStyle;
  }
  if (options.federation !== undefined) config['federation'] = options.federation;
  if (options.importBase !== undefined) config['importBase'] = options.importBase;
  return JSON.stringify(config);
}

/**
 * Generate the Treaty file-routing virtual module DURING a build. The single shared
 * entry every `@treaty` bundler plugin's `load` hook calls for
 * {@link RESOLVED_TREATY_ROUTES_ID}: it resolves the routes root to an absolute
 * path, drives the Rust `generateRoutes` core, and returns the emitted TypeScript
 * module plus its route-file watch dependencies.
 *
 * The bundler-side code is a thin shim (resolve options → call Rust → register
 * watch files); the routing itself is Rust-only, identical to the CLI's
 * `--emit ts`. Throws when the addon is unavailable so a misconfigured build fails
 * loudly instead of silently serving no routes.
 */
function generateRoutesModule(options: RoutesVirtualModuleOptions): GeneratedRoutesModule {
  const generator = loadRouteGenerator();
  if (generator === null) {
    throw new Error(
      '[treaty] cannot generate file routes: the "@treaty/authoring-node" addon ' +
        '(generateRoutes) is unavailable. Ensure @treaty/authoring-node is installed and built.',
    );
  }

  const base = options.cwd ?? process.cwd();
  const root = isAbsolute(options.routesRoot)
    ? options.routesRoot
    : resolve(base, options.routesRoot);

  const result = generator.generateRoutes(root, toConfigJson(options));
  const watchFiles = result.files.map((file) => resolve(root, file));
  return { code: toJsModule(result.code), files: result.files, watchFiles };
}

/** A top-level `import type { … } from '…'` statement the route emitter prepends. */
const TYPE_ONLY_IMPORT = /^\s*import\s+type\s+[^;\n]*?from\s*['"][^'"]+['"];?\s*$/gm;

/**
 * The `: Routes` type annotation on the emitted `export const routes` declaration.
 * Anchored to the declaration keyword + name so it can never match a `:` inside a
 * route path string (e.g. `"docs/:category"`).
 */
const ROUTES_TYPE_ANNOTATION = /(export\s+const\s+routes\b)\s*:\s*Routes\b/;

/** A trailing TS `as const` assertion (on the emitted `federationRemotes` array). */
const AS_CONST_ASSERTION = /\bas\s+const\b/g;

/**
 * Down-level the file-routing core's TypeScript route module to plain JS so EVERY
 * bundler can parse the `virtual:treaty-routes` module directly — the virtual
 * module has no on-disk path, which opts it out of a bundler's built-in TS
 * transform, so the shared shim must hand back JS rather than leaving each bundler
 * to bolt on its own transpile (previously an example-local `transformWithEsbuild`).
 *
 * The route emitter is the single, stable producer of this text, so its TS surface
 * is exactly: a leading `import type { Routes } from '@angular/router'`, the
 * `: Routes` annotation on `export const routes`, and a trailing `as const` on the
 * `federationRemotes` array. Each is stripped precisely (the annotation/`import`
 * matches are anchored so a `:` inside a route-path string is never touched),
 * leaving the route graph itself byte-for-byte unchanged.
 */
function toJsModule(code: string): string {
  return code
    .replace(TYPE_ONLY_IMPORT, '')
    .replace(ROUTES_TYPE_ANNOTATION, '$1')
    .replace(AS_CONST_ASSERTION, '');
}
