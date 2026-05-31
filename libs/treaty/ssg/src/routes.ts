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
 * The walk recurses into `children`, accumulating the URL prefix, so a child
 * route is enumerated at its full path. Wildcard (`**`) and pure-redirect routes
 * are never prerenderable and are dropped.
 */

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

/** A wildcard catch-all (`**`) is never a concrete prerenderable URL. */
function isWildcard(path: string | undefined): boolean {
	return (path ?? '').replace(/^\/+|\/+$/g, '') === '**'
}

/** The `:param` names declared in a route path (`'blog/:slug'` -> `['slug']`). */
function paramNames(routePath: string): string[] {
	return routePath
		.split('/')
		.filter((seg) => seg.startsWith(':'))
		.map((seg) => seg.slice(1))
}

/** Substitute `:param` segments in `routePath` using `params`; throws if missing. */
function substitute(routePath: string, params: RouteParams): string {
	const url = routePath
		.split('/')
		.map((seg) => {
			if (!seg.startsWith(':')) return seg
			const name = seg.slice(1)
			const value = params[name]
			if (value === undefined || value === '') {
				throw new RouteDiscoveryError(
					`route '${routePath}' is missing a value for parameter ':${name}'`
				)
			}
			return encodeURIComponent(value)
		})
		.join('/')
	return url
}

/** Error raised when a route cannot be enumerated into concrete URLs. */
export class RouteDiscoveryError extends Error {
	constructor(message: string) {
		super(message)
		this.name = 'RouteDiscoveryError'
	}
}

/** Normalize a discovered URL to a single leading slash (index -> `'/'`). */
function toUrl(rawPath: string): string {
	const clean = rawPath.replace(/^\/+|\/+$/g, '')
	return clean === '' ? '/' : `/${clean}`
}

/** Whether `route` is a concrete prerenderable node (not a wildcard/redirect). */
function isRenderable(route: RouteLike, includeComponentless: boolean): boolean {
	const wildcard = isWildcard(route.path)
	const pureRedirect = route.redirectTo !== undefined && !hasComponent(route)
	return (hasComponent(route) || includeComponentless) && !wildcard && !pureRedirect
}

/** Emit the single {@link DiscoveredRoute} for a static (non-parameterized) node. */
function staticRoute(route: RouteLike, here: string): DiscoveredRoute {
	return { url: toUrl(here), routePath: here, parameterized: false, params: {}, route }
}

/** Emit one {@link DiscoveredRoute} per param set for a parameterized node. */
function parameterizedRoutes(
	route: RouteLike,
	here: string,
	sets: readonly RouteParams[]
): DiscoveredRoute[] {
	return sets.map((params) => ({
		url: toUrl(substitute(here, params)),
		routePath: here,
		parameterized: true,
		params,
		route,
	}))
}

/** The static param sets declared for a route in {@link DiscoverRoutesOptions.params}. */
function staticParamSets(
	options: DiscoverRoutesOptions,
	route: RouteLike,
	here: string
): readonly RouteParams[] {
	return options.params?.[route.path ?? here] ?? options.params?.[here] ?? []
}

/**
 * Walk `routes`, accumulating the URL prefix, and collect every concrete
 * prerenderable route. Parameterized routes fan out into one entry per supplied
 * param set; static routes yield exactly one. Wildcards and pure redirects are
 * dropped. Children are always recursed so a renderable child of a layout route
 * is still discovered at its full path.
 */
function walk(
	routes: readonly RouteLike[],
	prefix: string,
	options: DiscoverRoutesOptions,
	out: DiscoveredRoute[]
): void {
	for (const route of routes) {
		const here = joinPath(prefix, route.path)

		if (isRenderable(route, options.includeComponentless === true)) {
			const names = paramNames(here)
			if (names.length === 0) {
				out.push(staticRoute(route, here))
			} else {
				out.push(...parameterizedRoutes(route, here, staticParamSets(options, route, here)))
			}
		}

		if (route.children && route.children.length > 0) {
			walk(route.children, here, options, out)
		}
	}
}

/**
 * Async sibling of {@link walk} that resolves each parameterized route's param
 * sets through a {@link StaticPathsProvider} (in addition to any static
 * `params`). Static nodes and recursion are identical to {@link walk}; only the
 * parameterized branch differs, awaiting the provider per route.
 */
async function walkAsync(
	routes: readonly RouteLike[],
	prefix: string,
	options: DiscoverRoutesAsyncOptions,
	out: DiscoveredRoute[]
): Promise<void> {
	for (const route of routes) {
		const here = joinPath(prefix, route.path)

		if (isRenderable(route, options.includeComponentless === true)) {
			const names = paramNames(here)
			if (names.length === 0) {
				out.push(staticRoute(route, here))
			} else {
				const sets = [...staticParamSets(options, route, here)]
				if (options.getStaticPaths) {
					const provided = await options.getStaticPaths({ routePath: here, params: names, route })
					if (provided) sets.push(...provided)
				}
				out.push(...parameterizedRoutes(route, here, sets))
			}
		}

		if (route.children && route.children.length > 0) {
			await walkAsync(route.children, here, options, out)
		}
	}
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
 */
export function discoverRoutes(
	routes: readonly RouteLike[] | undefined,
	options: DiscoverRoutesOptions = {}
): DiscoveredRoute[] {
	const out: DiscoveredRoute[] = []
	if (routes && routes.length > 0) walk(routes, '', options, out)
	return out
}

/**
 * Async variant of {@link discoverRoutes} that additionally resolves each
 * parameterized route's param sets through a `getStaticPaths`-style
 * {@link StaticPathsProvider} (see {@link DiscoverRoutesAsyncOptions}). Static
 * routes and non-parameterized output are identical to {@link discoverRoutes};
 * this is the entry the site generator uses so a route can compute its `:slug`
 * universe (from content, a CMS, a macro) at build time rather than only from a
 * hard-coded {@link RouteParamsMap}.
 */
export async function discoverRoutesAsync(
	routes: readonly RouteLike[] | undefined,
	options: DiscoverRoutesAsyncOptions = {}
): Promise<DiscoveredRoute[]> {
	const out: DiscoveredRoute[] = []
	if (routes && routes.length > 0) await walkAsync(routes, '', options, out)
	return out
}
