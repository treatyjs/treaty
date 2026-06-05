/**
 * @module
 *
 * **Static Ivy renderer**: turns the Ivy JS that `@treaty/compiler` emits for a
 * component into a static HTML string at build time, binding interpolations
 * against render data.
 *
 * Ivy lowers a template to a `*_Template(rf, ctx)` function whose body is a
 * deterministic instruction stream: a *create* block (`rf & 1`) of
 * `ɵɵdomElementStart` / `ɵɵtext` / `ɵɵdomElementEnd` / `ɵɵtext("literal")`
 * calls that build the DOM shape, and an *update* block (`rf & 2`) of
 * `ɵɵadvance()` / `ɵɵtextInterpolate(ctx.x)` calls that fill the dynamic text.
 * Because that stream is data — not behaviour — it can be replayed at build time
 * against the route's render data to emit the same HTML the browser would, with
 * no DOM, no zone, and no Angular runtime.
 *
 * The interpreter itself lives in the Rust SSG core (`treaty_ssg::ivy_html`, via
 * the `@treaty/ssg-node` addon); per [[rust-core-ts-shim-layering]] this module
 * is a thin marshalling wrapper. It covers the static-content + text-interpolation
 * subset SSG prerender targets; instructions outside that subset are ignored
 * rather than guessed at, so output is always a faithful subset of the live
 * render — never a wrong one — and the hydration marker tells the client runtime
 * to take over for the dynamic remainder.
 */

import { loadNative } from './native.js'
import type { RenderData } from './runtime.js'

/**
 * Render the emitted Ivy JS for a component to a static HTML fragment, binding
 * interpolations against `data`. Returns the empty string when `code` carries no
 * recognizable template function (a pass-through module), so callers can treat
 * "nothing to prerender" uniformly.
 *
 * The static interpretation runs in the Rust core; this wrapper only marshals
 * the render data to JSON and returns the fragment.
 *
 * @param code Ivy JS emitted by `@treaty/compiler` for one component.
 * @param data Render data (from the component's render-time macro, or `{}`).
 */
export function renderIvyToHtml(code: string, data: RenderData = {}): string {
	return loadNative().renderIvyToHtml(code, JSON.stringify(data))
}
