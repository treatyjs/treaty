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
 * exposed through Rspack's `loader.options` / plugin constructor.
 */
export type TreatyLoaderOptions = TreatyCompilerOptions

/** Options accepted by {@link TreatyRspackPlugin}. */
export interface TreatyPluginOptions extends TreatyCompilerOptions {
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

/** Default `test` matcher for the loader rule: any Treaty authoring extension. */
export const DEFAULT_TEST: RegExp = /\.(treaty|tsx|tjsx)$/
