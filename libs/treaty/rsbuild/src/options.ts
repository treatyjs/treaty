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
 * Default `test` regex matching every Treaty authoring extension
 * (`.treaty`/`.tsx`/`.tjsx`) PLUS plain `.ts` — so a `.ts` Angular decorated class
 * (`@Component`/`@Directive`/`@Pipe`/`@Injectable`/`@NgModule`) is lowered to its Ivy
 * definition — while never matching a `.d.ts` declaration file (the `(?<!\.d)`
 * look-behind).
 *
 * Matching every `.ts` is at PARITY with `@treaty/vite` (whose `classify` claims any
 * non-`.d.ts` `.ts`) and does NOT intercept the host's ordinary TypeScript pipeline:
 * ownership is decided one level down by the core compiler, not this regex. The
 * registered `api.transform` handler hands a matched `.ts` to `compiler.transform`,
 * which AST-screens for an Angular decorator and returns `null` for an ordinary `.ts`;
 * the handler then returns that file's source UNCHANGED, so Rsbuild's normal SWC `.ts`
 * loader transpiles it afterward exactly as before. An Angular `.ts` is the only kind
 * actually rewritten — so the transform over a non-Angular `.ts` is a cheap
 * classify + passthrough, not an interception.
 *
 * Without `.ts` here a decorated `.ts` `@Component` in a Treaty app on Rsbuild never
 * reached the transform and fell through to SWC's raw decorator transform — shipping a
 * decorated class with NO Ivy definition, so Angular dropped to its JIT compiler at
 * runtime. Recognising `.ts` keeps the whole graph AOT. Callers can still narrow this
 * via {@link TreatyPluginOptions.include}.
 */
export const DEFAULT_TEST = /(?<!\.d)\.(treaty|tsx|tjsx|ts)$/

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
