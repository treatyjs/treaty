/**
 * Rslib build for the LIBRARY variant of the everything-app.
 *
 * Treaty is a compiler, not a host: this config builds the showcase components
 * as a publishable library rather than an app bundle. The developer runs Rslib
 * over it (`rslib build`). `defineTreatyLib()` returns a complete `RslibConfig`
 * with:
 *
 *   - the Treaty transform wired in (lowers `.treaty` / `.tsx` / `.tjsx` /
 *     `@Component` `.ts` to Ivy JS via `@treaty/compiler` -> the Rust addon),
 *   - ESM output plus `.d.ts` declarations,
 *   - `@angular/*` externalized (a library never bundles the framework), and
 *   - file-by-file transpile (`bundle: false`) so the tree-shaking metadata the
 *     Treaty compiler emits survives to the downstream consumer.
 *
 * `@rslib/core` is a peer dependency the developer provides; `defineTreatyLib`
 * returns a structural `RslibConfig` (assignable to the real one) so this file
 * typechecks without the peer installed -- the same structural-peer discipline
 * the bundler plugins use. A library is consumed, not federated, so there is no
 * Module Federation here; federation is for the app builds (vite/rspack above).
 */
import { defineTreatyLib } from '@treaty/rslib'

export default defineTreatyLib({
	// ESM + .d.ts is the recommended shape for an Angular/Treaty library.
	formats: ['esm'],
	dts: true,
	// Transpile per file so Treaty's per-module sideEffects hints reach the
	// consumer's bundler for tree-shaking (this is the defineTreatyLib default,
	// stated explicitly here for the showcase).
	bundle: false,
	// `@angular/*` is always externalized; add the router on top since the
	// showcase routes import it.
	externals: ['@angular/router'],
})
