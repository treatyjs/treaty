/**
 * @module
 *
 * **Route discovery** for `@treaty/ssg`: given a routes config — the same
 * structural Angular `Routes` shape `@treaty/module-federation` consumes — it
 * enumerates the concrete routes that can be prerendered to static HTML at build
 * time.
 *
 * Two kinds of route are prerenderable:
 *   - **static** routes (`path: 'about'`, `path: ''`) — exactly one output;
 *   - **parameterized** routes (`path: 'blog/:slug'`) — one output per param set
 *     the caller supplies, because Treaty cannot know the universe of `:slug`
 *     values at build time. A parameterized route with no supplied params is
 *     skipped (it cannot be statically materialized).
 *
 * The deterministic walk — recursing into `children` with the URL prefix
 * accumulated, dropping wildcard (`**`) and pure-redirect routes, fanning
 * parameterized routes out and percent-encoding param values — runs in the Rust
 * core (`treaty_ssg::discovery`, via the `@treaty/ssg-node` addon). This module
 * is the thin host glue: it lowers the `RouteLike` config (which carries
 * function-valued loaders that cannot cross the JSON boundary) to the structural
 * `RouteSpec` the core needs, resolves any `getStaticPaths` callback (a host
 * function) into concrete param sets, calls the core, and re-attaches each route
 * node to the returned {@link DiscoveredRoute}s.
 */

import { loadNative } from './native.js'

/**
 * The minimal structural shape of an Angular route this package needs. It is a
 * deliberate subset of `@angular/router`'s `Route` (and identical in spirit to
 * `@treaty/module-federation`'s `RouteLike`) so a caller can pass a real
 * `Routes` array — or the route config it already feeds module-federation —
 * without an adapter; extra Angular fields are ignored.
 */
export interface RouteLike {
	/** The URL segment for this route (`''` is the empty/index path). */
	readonly path?: string
	/** Lazy standalone component loader (`loadComponent: () => import('…')`). */
	readonly loadComponent?: unknown
	/** Lazy routes/children loader (`loadChildren: () => import('…')`). */
	readonly loadChildren?: unknown
	/** Eagerly-referenced component — an eager (non-lazy) route. */
	readonly component?: unknown
	/** A redirect target; a route that only redirects is not prerenderable. */
	readonly redirectTo?: string
	/** Nested child routes, walked recursively. */
	readonly children?: readonly RouteLike[]
	/** Allow (and ignore) any other Angular `Route` fields. */
	readonly [extra: string]: unknown
}

/** A concrete parameter binding for one materialization of a route. */
export type RouteParams = Readonly<Record<string, string>>

/**
 * Caller-supplied parameter sets for parameterized routes, keyed by the route's
 * declared `path` (e.g. `'blog/:slug'`). Each entry is the list of `:param`
 * bindings to materialize. A route whose declared path is absent here yields no
 * output (Treaty has no way to enumerate its params).
 */
export type RouteParamsMap = Readonly<Record<string, readonly RouteParams[]>>

/** A single concrete, prerenderable route resolved from the config. */
export interface DiscoveredRoute {
	/**
	 * The concrete URL path with params substituted and a single leading slash
	 * (`'/'`, `'/about'`, `'/blog/hello-world'`). This is the route's identity in
	 * the prerender manifest.
	 */
	readonly url: string
	/** The declared route path before substitution (`'blog/:slug'`). */
	readonly routePath: string
	/** Whether the declared path carried any `:param` segments. */
	readonly parameterized: boolean
	/** The param bindings applied to produce {@link DiscoveredRoute.url}. */
	readonly params: RouteParams
	/** The route node from the config, for the pipeline to compile its component. */
	readonly route: RouteLike
}

/**
 * A `getStaticPaths`-style provider: given a parameterized route, return the
 * param sets to materialize at build time. This is the dynamic counterpart to a
 * static {@link RouteParamsMap} entry — a route can compute its `:slug` universe
 * from a CMS, a content directory, or a render-time macro instead of hard-coding
 * it. It is keyed by the route's declared path so a caller can branch per route.
 *
 * Returning `[]` (or `undefined`) for a route materializes none of it. Both the
 * declared path WITH the leading prefix (`'blog/:slug'`) and the bare segment
 * are passed so either lookup key resolves.
 */
export type StaticPathsProvider = (
	route: StaticPathsRequest
) => readonly RouteParams[] | undefined | Promise<readonly RouteParams[] | undefined>

/** The request a {@link StaticPathsProvider} receives for one parameterized route. */
export interface StaticPathsRequest {
	/** The full declared route path including parent prefix (`'blog/:slug'`). */
	readonly routePath: string
	/** The `:param` names declared in the path (`['slug']`). */
	readonly params: readonly string[]
	/** The route node from the config, so the provider can read its `data`/meta. */
	readonly route: RouteLike
}

/** Options controlling route discovery. */
export interface DiscoverRoutesOptions {
	/**
	 * Param sets for parameterized routes, keyed by declared path. Routes with
	 * `:params` and no entry here (and no {@link DiscoverRoutesOptions.getStaticPaths}
	 * result) are skipped.
	 */
	readonly params?: RouteParamsMap
	/**
	 * Include routes that have no component to render (pure layout/redirect
	 * shells with only `children`). Defaults to `false`: a route is only emitted
	 * when it carries a `component` or a lazy `loadComponent`. Layout routes are
	 * still walked for their children regardless of this flag.
	 */
	readonly includeComponentless?: boolean
}

/** Async options for {@link discoverRoutesAsync}, adding a static-paths provider. */
export interface DiscoverRoutesAsyncOptions extends DiscoverRoutesOptions {
	/**
	 * A `getStaticPaths`-style provider invoked for each parameterized route to
	 * compute its param sets at build time. Its results are appended to any static
	 * {@link DiscoverRoutesOptions.params} entry for the same route (static first,
	 * then provided), so a route can mix hard-coded and computed params.
	 */
	readonly getStaticPaths?: StaticPathsProvider
}

/** Error raised when a route cannot be enumerated into concrete URLs. */
export class RouteDiscoveryError extends Error {
	constructor(message: string) {
		super(message)
		this.name = 'RouteDiscoveryError'
	}
}

/** The structural `RouteSpec` the Rust core's discovery walk consumes. */
interface RouteSpec {
	readonly path: string
	readonly has_component: boolean
	readonly has_load_component: boolean
	readonly redirect_to?: string
	readonly children: RouteSpec[]
}

/** The `DiscoveredRoute` shape the Rust core returns (no `route` node). */
interface NativeDiscoveredRoute {
	readonly url: string
	readonly routePath: string
	readonly parameterized: boolean
	readonly params: RouteParams
}

/** Join a parent URL prefix with a child segment, normalizing slashes. */
function joinPath(parent: string, segment: string | undefined): string {
	const child = (segment ?? '').replace(/^\/+|\/+$/g, '')
	if (parent === '') return child
	if (child === '') return parent
	return `${parent}/${child}`
}

/** A route is renderable iff it has a component or a lazy component loader. */
function hasComponent(route: RouteLike): boolean {
	return route.component !== undefined || route.loadComponent !== undefined
}

/** The `:param` names declared in a route path (`'blog/:slug'` -> `['slug']`). */
function paramNames(routePath: string): string[] {
	return routePath
		.split('/')
		.filter((seg) => seg.startsWith(':'))
		.map((seg) => seg.slice(1))
}

/**
 * Lower a `RouteLike` tree to the structural {@link RouteSpec} tree the Rust
 * core needs (function-valued loaders reduced to the booleans the walk branches
 * on), while recording each accumulated declared path → route node so the node
 * can be re-attached to the core's component-free {@link DiscoveredRoute}s.
 */
function lowerRoutes(
	routes: readonly RouteLike[],
	prefix: string,
	nodeByPath: Map<string, RouteLike>
): RouteSpec[] {
	return routes.map((route) => {
		const here = joinPath(prefix, route.path)
		nodeByPath.set(here, route)
		const spec: RouteSpec = {
			path: route.path ?? '',
			has_component: route.component !== undefined,
			has_load_component: route.loadComponent !== undefined,
			redirect_to: route.redirectTo,
			children:
				route.children && route.children.length > 0
					? lowerRoutes(route.children, here, nodeByPath)
					: [],
		}
		return spec
	})
}

/**
 * Validate that every supplied param set binds all of a route's declared
 * `:param`s, throwing {@link RouteDiscoveryError} for a missing/empty binding.
 * Mirrors the historical TS `substitute` error so a malformed param set is a
 * loud failure rather than a silently-dropped route (the Rust walk would skip
 * it); the actual URL substitution + percent-encoding is the core's job.
 */
function validateParamSets(routePath: string, names: readonly string[], sets: readonly RouteParams[]): void {
	for (const params of sets) {
		for (const name of names) {
			const value = params[name]
			if (value === undefined || value === '') {
				throw new RouteDiscoveryError(
					`route '${routePath}' is missing a value for parameter ':${name}'`
				)
			}
		}
	}
}

/** Merge static + computed param sets into a single map keyed by declared path. */
function mergeParams(
	base: RouteParamsMap | undefined,
	extra: Record<string, RouteParams[]>
): Record<string, RouteParams[]> {
	const merged: Record<string, RouteParams[]> = {}
	for (const [key, value] of Object.entries(base ?? {})) merged[key] = [...value]
	for (const [key, value] of Object.entries(extra)) {
		merged[key] = [...(merged[key] ?? []), ...value]
	}
	return merged
}

/**
 * Re-attach the route node to each core-returned {@link DiscoveredRoute} by its
 * declared `routePath`, producing the public shape. A path with no recorded node
 * (should not happen for a core-discovered route) falls back to an empty node.
 */
function attachNodes(
	native: readonly NativeDiscoveredRoute[],
	nodeByPath: Map<string, RouteLike>
): DiscoveredRoute[] {
	return native.map((route) => ({
		url: route.url,
		routePath: route.routePath,
		parameterized: route.parameterized,
		params: route.params,
		route: nodeByPath.get(route.routePath) ?? {},
	}))
}

/**
 * Enumerate every concrete, prerenderable route from a routes config.
 *
 * Static routes (no `:param`) each yield one {@link DiscoveredRoute};
 * parameterized routes yield one per param set supplied in
 * {@link DiscoverRoutesOptions.params} (and none if no set is given — Treaty
 * cannot invent the param values). Wildcard (`**`) and pure-redirect routes are
 * excluded. Children are walked recursively, so the result is the full flat list
 * of URLs the prerender pipeline will materialize.
 *
 * The deterministic walk runs in the Rust SSG core; this function only lowers
 * the config, validates supplied param sets, and re-attaches the route nodes.
 */
export function discoverRoutes(
	routes: readonly RouteLike[] | undefined,
	options: DiscoverRoutesOptions = {}
): DiscoveredRoute[] {
	if (!routes || routes.length === 0) return []

	const nodeByPath = new Map<string, RouteLike>()
	const specs = lowerRoutes(routes, '', nodeByPath)

	// Validate the supplied static param sets for completeness (the loud-failure
	// contract) before the core fans them out.
	for (const [declaredPath, sets] of Object.entries(options.params ?? {})) {
		validateParamSets(declaredPath, paramNames(declaredPath), sets)
	}

	const native = loadNative()
	const json = native.discoverRoutes(
		JSON.stringify({
			routes: specs,
			params: options.params ?? {},
			includeComponentless: options.includeComponentless === true,
		})
	)
	const discovered = JSON.parse(json) as NativeDiscoveredRoute[]
	return attachNodes(discovered, nodeByPath)
}

/**
 * Async variant of {@link discoverRoutes} that additionally resolves each
 * parameterized route's param sets through a `getStaticPaths`-style
 * {@link StaticPathsProvider} (see {@link DiscoverRoutesAsyncOptions}). Static
 * routes and non-parameterized output are identical to {@link discoverRoutes};
 * this is the entry the site generator uses so a route can compute its `:slug`
 * universe (from content, a CMS, a macro) at build time rather than only from a
 * hard-coded {@link RouteParamsMap}.
 *
 * The `getStaticPaths` callback (a host function) is run here, in TS, per
 * parameterized route; its results are folded into the param map and the
 * deterministic fan-out is then delegated to the Rust core in one call.
 */
export async function discoverRoutesAsync(
	routes: readonly RouteLike[] | undefined,
	options: DiscoverRoutesAsyncOptions = {}
): Promise<DiscoveredRoute[]> {
	if (!routes || routes.length === 0) return []

	const nodeByPath = new Map<string, RouteLike>()
	const specs = lowerRoutes(routes, '', nodeByPath)

	// Resolve the getStaticPaths provider per parameterized route (the host
	// function seam Rust cannot run), accumulating computed sets keyed by path.
	const computed: Record<string, RouteParams[]> = {}
	if (options.getStaticPaths) {
		await resolveStaticPaths(routes, '', options, computed)
	}

	const params = mergeParams(options.params, computed)

	// Validate completeness (static + computed) before the core fans out.
	for (const [declaredPath, sets] of Object.entries(params)) {
		validateParamSets(declaredPath, paramNames(declaredPath), sets)
	}

	const native = loadNative()
	const json = native.discoverRoutes(
		JSON.stringify({
			routes: specs,
			params,
			includeComponentless: options.includeComponentless === true,
		})
	)
	const discovered = JSON.parse(json) as NativeDiscoveredRoute[]
	return attachNodes(discovered, nodeByPath)
}

/**
 * Walk the `RouteLike` tree (TS-side, since it carries the route nodes the
 * provider reads) and invoke `getStaticPaths` for each renderable parameterized
 * route, accumulating the computed param sets keyed by the route's full declared
 * path. Mirrors the core's renderability rule so the provider is asked about
 * exactly the routes the core will fan out.
 */
async function resolveStaticPaths(
	routes: readonly RouteLike[],
	prefix: string,
	options: DiscoverRoutesAsyncOptions,
	out: Record<string, RouteParams[]>
): Promise<void> {
	for (const route of routes) {
		const here = joinPath(prefix, route.path)
		const renderable =
			(hasComponent(route) || options.includeComponentless === true) &&
			(route.path ?? '').replace(/^\/+|\/+$/g, '') !== '**' &&
			!(route.redirectTo !== undefined && !hasComponent(route))
		if (renderable) {
			const names = paramNames(here)
			if (names.length > 0 && options.getStaticPaths) {
				const provided = await options.getStaticPaths({ routePath: here, params: names, route })
				if (provided && provided.length > 0) {
					out[here] = [...(out[here] ?? []), ...provided]
				}
			}
		}
		if (route.children && route.children.length > 0) {
			await resolveStaticPaths(route.children, here, options, out)
		}
	}
}
