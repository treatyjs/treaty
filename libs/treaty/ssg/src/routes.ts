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

/** Options controlling route discovery. */
export interface DiscoverRoutesOptions {
	/**
	 * Param sets for parameterized routes, keyed by declared path. Routes with
	 * `:params` and no entry here are skipped.
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

		const wildcard = isWildcard(route.path)
		const pureRedirect = route.redirectTo !== undefined && !hasComponent(route)
		const renderable = (hasComponent(route) || options.includeComponentless === true) && !wildcard && !pureRedirect

		if (renderable) {
			const names = paramNames(here)
			if (names.length === 0) {
				out.push({
					url: toUrl(here),
					routePath: here,
					parameterized: false,
					params: {},
					route,
				})
			} else {
				const sets = options.params?.[route.path ?? here] ?? options.params?.[here] ?? []
				for (const params of sets) {
					out.push({
						url: toUrl(substitute(here, params)),
						routePath: here,
						parameterized: true,
						params,
						route,
					})
				}
			}
		}

		if (route.children && route.children.length > 0) {
			walk(route.children, here, options, out)
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
