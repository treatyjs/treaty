/**
 * @module
 *
 * The Angular partial-declaration linker for `@treaty/rslib`.
 *
 * rslib builds on rsbuild, so the linker is wired the same way as in `@treaty/rsbuild`: through an
 * rsbuild `api.transform` registration matched to published `node_modules` `.mjs`/`.js` modules.
 * Published Angular libraries (`node_modules/@angular/* /fesm2022/*.mjs`) ship *partial*-compiled
 * (`ɵɵngDeclare*` calls); a Treaty library that re-exports or depends on them must de-partial them
 * to AOT `ɵɵdefine*` so a downstream app needs NO JIT and NO `@angular/compiler`.
 *
 * It reuses the EXACT shared linker core from `@treaty/ts-vite`
 * ({@link isPartialModule} + {@link linkPartialCode}) — the same bundler-agnostic, Rust-backed
 * (`@treaty/authoring-node`.`linkPartial`) functions `@treaty/vite`, `@treaty/rspack`, and
 * `@treaty/rsbuild` consume. Only the per-bundler registration differs; no linking logic is
 * re-implemented here.
 */

import { isPartialModule, linkPartialCode } from '@treaty/ts-vite'
import type {
	TreatyRsbuildPluginApi,
	TreatyTransformContext,
	TreatyTransformOutput,
} from './types.js'

/**
 * The `test` the linker transform matches: published `node_modules` `.mjs`/`.js`/`.cjs` modules.
 * The cheap per-module {@link isPartialModule} substring guard inside {@link linkPartialCode} keeps
 * the (more expensive) link off any matched file that is not actually partial-compiled. Exported so
 * callers wiring the transform themselves reuse the exact same matcher.
 */
export const LINK_PARTIAL_TEST: RegExp = /[\\/]node_modules[\\/].*\.[cm]?js$/

/**
 * Register the Angular partial-declaration linker on an rslib/rsbuild plugin API. Called from
 * {@link treatyRsbuildPlugin}'s setup so a Treaty library build links the published partial Angular
 * libraries it pulls in. Independent of the authoring transform: it only touches partial
 * `node_modules` modules.
 *
 * Linking is a span rewrite that preserves byte offsets outside the rewritten `ɵɵngDeclare*` calls,
 * so no source map is fabricated for these vendored libraries.
 */
export function registerLinkPartialTransform(api: TreatyRsbuildPluginApi): void {
	api.transform(
		{ test: LINK_PARTIAL_TEST },
		(context: TreatyTransformContext): TreatyTransformOutput | string => {
			const id = context.resource
			// Fast bail before touching the linker: only published modules carrying a ɵɵngDeclare* call.
			if (!isPartialModule(id, context.code)) return context.code
			const linked = linkPartialCode(context.code, id)
			// `null` here means the linker addon is unavailable: serve the source unchanged.
			return linked === null ? context.code : { code: linked }
		}
	)
}

export { isPartialModule }
