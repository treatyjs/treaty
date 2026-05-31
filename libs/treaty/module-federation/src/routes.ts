/**
 * @module
 *
 * Federation is Treaty's unit of **deployment granularity**: every lazy feature
 * route and every library is meant to be an independently versioned, deployable,
 * rollback-able federated module — flipped on the serving platform without
 * redeploying the whole app. For that to be zero-config the developer must not
 * hand-write an `exposes` map; Treaty derives it from the artifacts it already
 * understands: the Angular route graph and the workspace libraries.
 *
 * This module is the pure, framework-agnostic derivation: given an Angular
 * routes config (the `Routes` array, possibly nested) it produces a federation
 * `exposes` map in which each **lazy** route becomes one remote module entry,
 * and given a list of library entries it produces one exposed module per lib.
 * {@link generateMfConfig} merges these so a built Treaty app auto-exposes its
 * lazy routes + libs as remotes by default, with the manual `exposes` (if any)
 * still winning.
 */

/**
 * The minimal shape of an Angular route this package needs to decide whether a
 * route is lazy and what its public path is. It is a structural subset of
 * `@angular/router`'s `Route` so callers can pass their real `Routes` array
 * without an adapter — the extra fields Angular carries are simply ignored.
 */
export interface RouteLike {
	/** The URL segment for this route (`''` is the empty/index path). */
	readonly path?: string
	/**
	 * Lazy standalone component loader (`loadComponent: () => import('…')`).
	 * Its presence marks the route as a lazy, separately-loadable boundary.
	 */
	readonly loadComponent?: unknown
	/**
	 * Lazy routes/children loader (`loadChildren: () => import('…')`). Like
	 * {@link RouteLike.loadComponent}, its presence marks a lazy boundary.
	 */
	readonly loadChildren?: unknown
	/** Eagerly-referenced component — an eager (non-lazy) route. */
	readonly component?: unknown
	/** Nested child routes, walked recursively to find lazy boundaries. */
	readonly children?: readonly RouteLike[]
	/** Allow (and ignore) any other Angular `Route` fields. */
	readonly [extra: string]: unknown
}

/**
 * A workspace library to expose as a federated module. Either a bare module
 * path string (its public name is derived from the path) or a `{ name, path }`
 * pair when the exposed key should differ from the file path.
 */
export type LibEntry =
	| string
	| {
			/** Public name used to build the expose key (`./libs/<name>`). */
			readonly name: string
			/** Local module path that backs the exposed entry. */
			readonly path: string
	  }

/** Options controlling how route paths map to expose keys / module paths. */
export interface DeriveRoutesOptions {
	/**
	 * Prefix for the generated expose **keys**. Defaults to `./routes`, so a
	 * route `path: 'dashboard'` is exposed as `./routes/dashboard`.
	 */
	readonly keyPrefix?: string
	/**
	 * Base directory used to synthesize a module **path** for a lazy route when
	 * the loader's import specifier cannot be statically read (it is a function
	 * at runtime). Defaults to `./src/app`, so `dashboard` →
	 * `./src/app/dashboard`. The serving platform resolves the real artifact;
	 * this is the stable logical handle Treaty exposes.
	 */
	readonly pathBase?: string
}

/** Options controlling how libraries map to expose keys / module paths. */
export interface DeriveLibsOptions {
	/**
	 * Prefix for the generated expose **keys**. Defaults to `./libs`, so a lib
	 * named `data-access` is exposed as `./libs/data-access`.
	 */
	readonly keyPrefix?: string
}

/** Default expose-key prefix for derived routes. */
export const DEFAULT_ROUTE_KEY_PREFIX = './routes'

/** Default base dir used to synthesize a lazy route's exposed module path. */
export const DEFAULT_ROUTE_PATH_BASE = './src/app'

/** Default expose-key prefix for derived libraries. */
export const DEFAULT_LIB_KEY_PREFIX = './libs'

/** A route is lazy iff it carries a `loadComponent` or `loadChildren` loader. */
function isLazyRoute(route: RouteLike): boolean {
	return route.loadComponent !== undefined || route.loadChildren !== undefined
}

/**
 * Normalize an arbitrary route path segment into the single, slug-like segment
 * used in expose keys and module paths. Strips leading/trailing slashes and
 * collapses parameter/wildcard syntax so keys stay stable and file-safe:
 *   `''` → `index`, `'**'` → `wildcard`, `':id'` → `id`, `'a/b'` → `a/b`.
 */
function normalizeSegment(path: string | undefined): string {
	const raw = (path ?? '').replace(/^\/+|\/+$/g, '')
	if (raw === '') return 'index'
	if (raw === '**') return 'wildcard'
	return raw
		.split('/')
		.map((seg) => (seg.startsWith(':') ? seg.slice(1) : seg))
		.filter((seg) => seg.length > 0)
		.join('/')
}

/**
 * Join a parent path prefix with a child segment into a normalized,
 * slash-separated path used for nested lazy routes. Empty parents/children are
 * dropped so an empty-path child of `feature` stays `feature` rather than
 * `feature/index`.
 */
function joinPath(parent: string, segment: string | undefined): string {
	const child = (segment ?? '').replace(/^\/+|\/+$/g, '')
	if (parent === '') return child
	if (child === '') return parent
	return `${parent}/${child}`
}

/**
 * Walk a routes array (recursing into eager `children`) and collect every lazy
 * route as an `{ path }` whose path is the accumulated, normalized URL prefix.
 * Lazy `loadChildren` boundaries are treated as leaves: their internal children
 * load as part of that remote, so we do not descend past them.
 */
function collectLazyRoutes(
	routes: readonly RouteLike[],
	prefix: string,
	out: { rawPath: string }[]
): void {
	for (const route of routes) {
		const here = joinPath(prefix, route.path)
		if (isLazyRoute(route)) {
			out.push({ rawPath: here })
			// `loadChildren` owns its own subtree; do not descend into eager
			// `children` of a lazy boundary (a lazy route rarely has them, but
			// guard regardless so we never double-expose).
			continue
		}
		if (route.children && route.children.length > 0) {
			collectLazyRoutes(route.children, here, out)
		}
	}
}

/**
 * Derive a federation `exposes` map from an Angular routes config. Each **lazy**
 * route (one with `loadComponent` or `loadChildren`) becomes one entry; eager
 * routes (plain `component`, redirects, layout shells) are not exposed because
 * they are not independently deployable boundaries.
 *
 * Keys are `${keyPrefix}/${path}` (default prefix `./routes`); the value is a
 * synthesized, stable module path under `pathBase` (default `./src/app`). This
 * is the logical handle the serving platform maps to a concrete, versioned
 * artifact — the whole point of federation-as-deployment-granularity.
 *
 * Nested eager routes are walked so a lazy child of an eager layout route is
 * still exposed under its full path. The empty (`''`) path becomes `index` and
 * the wildcard (`**`) becomes `wildcard` so keys are always file-safe.
 */
export function deriveExposesFromRoutes(
	routes: readonly RouteLike[] | undefined,
	options: DeriveRoutesOptions = {}
): Record<string, string> {
	const keyPrefix = (options.keyPrefix ?? DEFAULT_ROUTE_KEY_PREFIX).replace(/\/+$/g, '')
	const pathBase = (options.pathBase ?? DEFAULT_ROUTE_PATH_BASE).replace(/\/+$/g, '')

	const collected: { rawPath: string }[] = []
	if (routes && routes.length > 0) {
		collectLazyRoutes(routes, '', collected)
	}

	const exposes: Record<string, string> = {}
	for (const { rawPath } of collected) {
		const segment = normalizeSegment(rawPath)
		const key = `${keyPrefix}/${segment}`
		// Last lazy route at a given normalized path wins; routes that normalize
		// to the same key (e.g. `''` and `'/'`) collapse intentionally.
		exposes[key] = `${pathBase}/${segment}`
	}
	return exposes
}

/**
 * Derive a federation `exposes` map from a list of workspace libraries. Each lib
 * becomes one exposed module so it can be versioned and deployed independently
 * of the apps that consume it. Keys are `${keyPrefix}/${name}` (default prefix
 * `./libs`); a bare string entry uses its trailing path segment as the name.
 */
export function deriveExposesFromLibs(
	libs: readonly LibEntry[] | undefined,
	options: DeriveLibsOptions = {}
): Record<string, string> {
	const keyPrefix = (options.keyPrefix ?? DEFAULT_LIB_KEY_PREFIX).replace(/\/+$/g, '')
	const exposes: Record<string, string> = {}
	if (!libs || libs.length === 0) return exposes

	for (const lib of libs) {
		if (typeof lib === 'string') {
			const path = lib.replace(/\/+$/g, '')
			const name = path.split('/').filter(Boolean).pop() ?? path
			exposes[`${keyPrefix}/${name}`] = path
		} else {
			exposes[`${keyPrefix}/${lib.name}`] = lib.path
		}
	}
	return exposes
}
