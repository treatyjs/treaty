/**
 * @module
 *
 * Typed options for {@link pluginTreaty}. These are a thin, bundler-facing
 * layer over {@link TreatyCompilerOptions} from `@treaty/compiler`, plus the
 * extension/matching knobs that are specific to wiring the compiler into an
 * Rsbuild build.
 */

import type { TreatyCompilerOptions } from '@treaty/compiler'
import type { RoutesVirtualModuleOptions } from '@treaty/ts-vite'

/** The authoring file extensions Treaty owns and lowers to Ivy JS. */
export const TREATY_EXTENSIONS = ['.treaty', '.tsx', '.tjsx'] as const

/**
 * Default `test` regex matching every Treaty authoring extension. Plain `.ts`
 * `@Component` sources are intentionally *not* matched here: the core compiler
 * pre-screens `.ts` for an `@Component` decorator, but matching every `.ts`
 * file in an Rsbuild build would intercept the host's ordinary TypeScript
 * pipeline. Callers who want `.ts` `@Component` lowering can widen
 * {@link TreatyPluginOptions.include} explicitly.
 */
export const DEFAULT_TEST = /\.(treaty|tsx|tjsx)$/

/** Options accepted by {@link pluginTreaty}. */
export interface TreatyPluginOptions extends TreatyCompilerOptions {
	/**
	 * Override the module-id matcher. Defaults to {@link DEFAULT_TEST} (every
	 * Treaty authoring extension). Provide a custom {@link RegExp} to broaden or
	 * narrow which modules are routed through the Treaty compiler.
	 */
	readonly include?: RegExp
	/**
	 * Extensions to append to the bundler's `resolve.extensions` so bare imports
	 * of Treaty modules (`import X from './x'`) resolve. Defaults to
	 * {@link TREATY_EXTENSIONS}.
	 */
	readonly extensions?: readonly string[]
	/**
	 * Cold-build prewarm: absolute paths to owned authoring files to batch-compile
	 * up front via the core's `transformMany` (one parallel round trip through the
	 * Rust addon). Wired to rsbuild's `onBeforeBuild` cold-build hook when the host
	 * exposes it, so the per-module loader/transform calls during the build are
	 * served from the cache. No-op for the dev server or when the list is empty.
	 *
	 * rsbuild's transform/loader pipeline is pull-based per module with no hook
	 * that hands the plugin the full owned-file set, so this batch path is opt-in.
	 */
	readonly prewarm?: readonly string[]
	/**
	 * File-system routing as a VIRTUAL MODULE, generated DURING the build (no
	 * checked-in / prebuilt `routes.ts`). When set, the plugin aliases
	 * `import routes from 'virtual:treaty-routes'` to an in-package sentinel and runs
	 * the routes loader over it, which drives the Rust file-routing core
	 * (`@treaty/authoring-node`.`generateRoutes`) over the configured `routesRoot` so
	 * the route graph always reflects the on-disk `routes/` tree. Omitted ⇒ the
	 * virtual module is not wired (apps that do not use file routing are unaffected).
	 *
	 * The routing logic lives ONCE in Rust; the plugin is the thin Rsbuild shim
	 * (alias + Rspack loader rule), mirroring the partial-declaration linker.
	 */
	readonly fileRoutes?: RoutesVirtualModuleOptions
}

/** Split plugin options into the compiler core options it forwards. */
export function toCompilerOptions(options: TreatyPluginOptions): TreatyCompilerOptions {
	const { cache, annotatePure, dropUnusedServerFns } = options
	return { cache, annotatePure, dropUnusedServerFns }
}
