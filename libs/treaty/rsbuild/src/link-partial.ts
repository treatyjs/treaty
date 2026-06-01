/**
 * @module
 *
 * The Angular partial-declaration linker, wired as an Rsbuild plugin via `api.transform`.
 *
 * Published Angular libraries (`node_modules/@angular/* /fesm2022/*.mjs`) ship *partial*-compiled:
 * every decorated class emits a `ɵɵngDeclare*({...})` call. Left un-linked, Angular falls back to
 * the JIT compiler at runtime and throws "needs JIT / `@angular/compiler` not available" the moment
 * `@angular/compiler` is excluded. This plugin rewrites those declarations to their AOT
 * `ɵɵdefine*` form so NO JIT and NO `@angular/compiler` are needed.
 *
 * It reuses the EXACT shared linker core from `@treaty/ts-vite`
 * ({@link isPartialModule} + {@link linkPartialCode}) — the same bundler-agnostic, Rust-backed
 * (`@treaty/authoring-node`.`linkPartial`) functions `@treaty/vite` (Vite) and `@treaty/rspack`
 * (loader) consume. Only the per-bundler registration differs: here it is an `api.transform`
 * matched to published `node_modules` `.mjs`/`.js` modules. No linking logic is re-implemented.
 *
 * Treaty is a compiler, not a host: the handler is a thin shim (guard + delegate). Modules that are
 * not partial-compiled (or when the linker addon is unavailable) are returned unchanged.
 */

import type { RsbuildPlugin, RsbuildPluginAPI, TransformContext } from '@rsbuild/core'
import { isPartialModule, linkPartialCode } from '@treaty/ts-vite'

/** Stable plugin name, asserted by the smoke test. */
export const LINK_PARTIAL_PLUGIN_NAME = 'treaty:rsbuild:link-partial'

/**
 * The `test` the linker transform matches: published `node_modules` `.mjs`/`.js`/`.cjs` modules.
 * The cheap per-module {@link isPartialModule} substring guard inside {@link linkPartialCode} keeps
 * the (more expensive) link off any matched file that is not actually partial-compiled. Exported so
 * callers wiring the transform themselves reuse the exact same matcher.
 */
export const LINK_PARTIAL_TEST: RegExp = /[\\/]node_modules[\\/].*\.[cm]?js$/

/**
 * Register the Angular partial-declaration linker on an Rsbuild plugin API. Shared by the standalone
 * {@link pluginTreatyLinkPartial} and folded into `pluginTreaty` so a Treaty Rsbuild build links
 * published partial Angular libraries automatically.
 *
 * Linking is a span rewrite that preserves byte offsets outside the rewritten `ɵɵngDeclare*` calls,
 * so no source map is fabricated for these vendored libraries (the original positions still hold).
 */
export function registerLinkPartialTransform(api: RsbuildPluginAPI): void {
	api.transform({ test: LINK_PARTIAL_TEST }, (context: TransformContext) => {
		const id = context.resourcePath
		// Fast bail before touching the linker: only published modules carrying a ɵɵngDeclare* call.
		if (!isPartialModule(id, context.code)) return context.code
		const linked = linkPartialCode(context.code, id)
		// `null` here means the linker addon is unavailable: serve the source unchanged.
		return linked === null ? context.code : { code: linked }
	})
}

/**
 * Standalone Rsbuild plugin that ONLY links partial Angular libraries (no authoring transform). Use
 * it in a build that does not route authoring files through `pluginTreaty` but still consumes
 * published partial-compiled Angular packages.
 */
export function pluginTreatyLinkPartial(): RsbuildPlugin {
	return {
		name: LINK_PARTIAL_PLUGIN_NAME,
		setup(api: RsbuildPluginAPI): void {
			registerLinkPartialTransform(api)
		},
	}
}

export { isPartialModule }
