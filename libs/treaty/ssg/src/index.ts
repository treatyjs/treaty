/**
 * @module
 *
 * Public API of `@treaty/ssg` — Static Site Generation for Treaty apps:
 * prerender routes to static HTML at BUILD time.
 *
 * Treaty is a compiler, not a host. `@treaty/ssg` takes an app's routes config
 * (the Angular `Routes` / `@treaty/module-federation` route shape), discovers
 * the prerenderable routes (static + parameterized), compiles each route's
 * component to Ivy through `@treaty/compiler`, executes any top-of-file ```
 * macro / RSC render-time data through the Nova runtime (via the
 * {@link RenderRuntime} seam — a Nova-backed `run_macro` impl plugs straight in,
 * with a built-in stub for builds without the native entry), statically
 * interprets the emitted Ivy template against that data, and emits a static,
 * hydration-ready HTML document per route plus a route → output-file
 * {@link PrerenderManifest}. {@link prerenderAll} is the one-call entry point.
 *
 * SSG only EMITS static output; serving it and bootstrapping client hydration
 * (Angular's `@angular/platform-browser` hydration / `@angular/platform-server`,
 * referenced structurally as optional peers) is the dev server's / platform's
 * job.
 */

// Route discovery.
export {
	discoverRoutes,
	RouteDiscoveryError,
	type RouteLike,
	type RouteParams,
	type RouteParamsMap,
	type DiscoveredRoute,
	type DiscoverRoutesOptions,
} from './routes.js'

// Render-time data seam (Nova `run_macro` plug-in point) + stub.
export {
	StubRenderRuntime,
	createNovaRenderRuntime,
	RenderRuntimeError,
	type RenderRuntime,
	type RenderMacro,
	type RenderData,
	type JsonValue,
	type NovaMacroAddon,
} from './runtime.js'

// Static Ivy → HTML renderer.
export { renderIvyToHtml } from './render.js'

// Prerender pipeline + entry point.
export {
	prerenderAll,
	prerenderRoute,
	PrerenderError,
	DEFAULT_OUT_DIR,
	HYDRATION_MARKER_ATTR,
	HYDRATION_STATE_ID,
	type PrerenderConfig,
	type PrerenderManifest,
	type PrerenderedRoute,
	type RoutePrerenderInput,
	type ResolveRouteInput,
} from './prerender.js'
