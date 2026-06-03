/**
 * @module
 *
 * Shared option types for `@treaty/rspack`. The loader and the plugin accept the
 * same Treaty compiler knobs, forwarded verbatim to `@treaty/compiler`. Keeping
 * them here means neither `loader.ts` nor `plugin.ts` has to import the other.
 */

import type { TreatyCompilerOptions } from '@treaty/compiler'
import type { MfOptions } from '@treaty/module-federation'
import type { RoutesVirtualModuleOptions } from '@treaty/ts-vite'

/**
 * Options understood by both the Treaty Rspack loader and plugin. These are the
 * {@link TreatyCompilerOptions} (cache, pure annotation, server-fn dropping)
 * exposed through Rspack's `loader.options` / plugin constructor, plus the
 * loader-only {@link TreatyLoaderOptions.selectorRoot} below.
 */
export interface TreatyLoaderOptions extends TreatyCompilerOptions {
	/**
	 * CROSS-MODULE SELECTOR RESOLUTION. The project root whose first-party `.ts`
	 * sources are scanned ONCE (via the Rust selector scanner) to resolve a parent
	 * component's template tags to an IMPORTED child used by its REAL `@Component`
	 * selector — the conventional Angular-CLI shape (`class StatCard` with
	 * `selector: 'app-stat-card'`, used as `<app-stat-card>`) that the class-name↔tag
	 * fold cannot match and which otherwise renders as an empty host.
	 *
	 *   - a string: the absolute (or cwd-relative) directory to scan.
	 *   - `true`: scan the current working directory.
	 *   - omitted / `false`: no cross-module scan; every file uses the byte-identical
	 *     class-name fold (the prior behaviour — strictly ADDITIVE).
	 *
	 * Rspack/webpack is a pull pipeline with no cold-build hook that hands a plugin
	 * the full owned-file set up front, so — mirroring `@treaty/vite`'s `buildStart`
	 * prewarm — the loader scans the root ONCE (idempotent, on the shared compiler
	 * instance) the first time it sees this option, then each per-file `transform`
	 * derives that file's `{ importName -> selector }` registry from the project map.
	 * A file whose imports resolve to NO known selector folds exactly as before.
	 */
	readonly selectorRoot?: string | boolean
}

/** Options accepted by {@link TreatyRspackPlugin}. */
export interface TreatyPluginOptions extends TreatyLoaderOptions {
	/**
	 * Authoring extensions to add to `resolve.extensions` so bare imports of
	 * Treaty modules resolve. Defaults to `['.treaty', '.tsx', '.tjsx']`.
	 */
	readonly extensions?: readonly string[]
	/**
	 * The `test` regular expression the generated module rule matches against. By
	 * default it matches Treaty's owned extensions; override to narrow or widen it.
	 */
	readonly test?: RegExp
	/**
	 * Automatic Module Federation. Every Treaty app is a Module Federation host
	 * by default — when this is enabled the plugin generates the federation
	 * config and adds the `@module-federation/enhanced` `ModuleFederationPlugin`
	 * automatically, so the developer writes no federation config by hand.
	 *
	 *   - `true` (the default): enable with the zero-config defaults (a host that
	 *     shares the Angular runtime as eager singletons).
	 *   - an {@link MfOptions} object: declare the app name, the remotes it
	 *     consumes, the modules it exposes, and extra shared deps.
	 *   - `false`: disable federation entirely (the loader/resolve wiring is
	 *     unaffected — fully backward compatible).
	 */
	readonly moduleFederation?: MfOptions | boolean
	/**
	 * File-system routing as a VIRTUAL MODULE, generated DURING the build (no
	 * checked-in / prebuilt `routes.ts`). When set, the plugin aliases
	 * `import routes from 'virtual:treaty-routes'` to an in-package sentinel and runs
	 * the routes loader over it, which drives the Rust file-routing core
	 * (`@treaty/authoring-node`.`generateRoutes`) over the configured `routesRoot` so
	 * the route graph always reflects the on-disk `routes/` tree. Omitted ⇒ the
	 * virtual module is not wired (apps that do not use file routing are unaffected).
	 *
	 * The routing logic lives ONCE in Rust; the plugin is the thin Rspack shim
	 * (alias + loader rule), mirroring the partial-declaration linker.
	 */
	readonly fileRoutes?: RoutesVirtualModuleOptions
}

/** The authoring extensions Treaty owns and the plugin resolves by default. */
export const DEFAULT_EXTENSIONS: readonly string[] = ['.treaty', '.tsx', '.tjsx']

/**
 * Default `test` matcher for the loader rule: every Treaty authoring extension
 * (`.treaty`/`.tsx`/`.tjsx`) PLUS plain `.ts` (so a `.ts` `@Component`/`@Directive`/
 * `@Pipe`/`@Injectable`/`@NgModule` is lowered to Ivy), but never a `.d.ts`
 * declaration file.
 *
 * Matching every `.ts` mirrors `@treaty/vite` (whose `classify` claims any non-`.d.ts`
 * `.ts`) and is SAFE because ownership is decided one level down by the core compiler,
 * not by this regex: {@link treatyLoader} hands the file to `compiler.transform`, which
 * AST-screens for an Angular decorator and returns `null` for an ordinary `.ts`. The
 * loader then returns that source UNCHANGED, so a non-Angular `.ts` falls straight
 * through to Rspack/webpack's own TS pipeline (e.g. the host's `builtin:swc-loader`) —
 * the rule running over it is a cheap classify + passthrough, not an interception.
 *
 * The `(?<!\.d)` look-behind excludes `.d.ts`: a declaration file carries no runtime
 * component to lower and must reach the host TS pipeline untouched.
 *
 * Without `.ts` here a decorated `.ts` `@Component` in a Treaty app on Rspack never
 * reached the loader and fell through to a raw decorator transform — shipping a
 * decorated class with NO Ivy definition, so Angular dropped to its JIT compiler at
 * runtime. Recognising `.ts` keeps the whole graph AOT, at parity with Vite.
 */
export const DEFAULT_TEST: RegExp = /(?<!\.d)\.(treaty|tsx|tjsx|ts)$/
