/**
 * Ambient declaration for the bundler-agnostic Treaty server-fn dev backend that
 * `@treaty/rsbuild` reuses from `@treaty/vite`.
 *
 * The server-fn dev backend (request parsing, arg decoding, SSR-execution of the real
 * body, JSON/SSE replies) is bundler-agnostic connect middleware that already lives,
 * tested, in `@treaty/vite`'s `dev-backend.ts` and is re-exported from its package
 * entry. `@treaty/rsbuild` reuses that ONE implementation rather than re-implementing
 * it; only the per-bundler registration differs (Rsbuild's `dev.setupMiddlewares`
 * instead of Vite's connect stack). `@treaty/vite`'s published types are not part of
 * `@treaty/rsbuild`'s composite program, so — mirroring `ts-vite-peer.d.ts` — we
 * declare only the dev-backend surface this package calls. The Vite-shaped plugin
 * exports of `@treaty/vite` are intentionally NOT declared here.
 */
declare module '@treaty/vite' {
	/** Server route prefix every backend registers a fn under (`/__server/<name>`). */
	export const SERVER_ROUTE_PREFIX: string

	/**
	 * A registered dev server fn: the module id whose SSR-loaded export carries the real
	 * body, keyed in the registry by the fn's export name (also the route segment).
	 */
	export interface DevServerFn {
		readonly exportName: string
		readonly moduleId: string
	}

	/** A connect-style middleware `req`: a method, a url, and a readable body stream. */
	export interface DevReq {
		method?: string
		url?: string
		on(event: 'data', cb: (chunk: Buffer | string) => void): void
		on(event: 'end', cb: () => void): void
		on(event: 'error', cb: (err: unknown) => void): void
	}

	/** A connect-style middleware `res`. */
	export interface DevRes {
		statusCode: number
		setHeader(name: string, value: string): void
		end(body?: string): void
		write(chunk: string): boolean
	}

	/** A connect-style middleware function. */
	export type DevConnectMiddleware = (
		req: DevReq,
		res: DevRes,
		next: (err?: unknown) => void
	) => void

	/**
	 * The minimal dev-server surface the shared middleware needs: a connect stack to
	 * register on and an SSR module loader to run the real body with.
	 */
	export interface DevBackendServer {
		readonly middlewares: { use(fn: DevConnectMiddleware): void }
		ssrLoadModule(id: string): Promise<Record<string, unknown>>
		ssrFixStacktrace?(e: unknown): void
	}

	/** Parse the `/__server/<name>` route segment out of a request url, or `null`. */
	export function parseServerFnRoute(url: string | undefined): string | null

	/** Decode the POSTed argument list into a positional argument array. */
	export function decodeArgs(body: string): unknown[]

	/**
	 * Build the dev backend middleware over a registry of server fns: matches
	 * `/__server/<name>`, decodes the POSTed args, `ssrLoadModule`s the fn's module to
	 * run the real body, and replies (JSON, or SSE for an async generator). A
	 * non-`/__server` request, or an unregistered name, falls through / 404s.
	 */
	export function createServerFnMiddleware(
		server: DevBackendServer,
		registry: ReadonlyMap<string, DevServerFn>
	): DevConnectMiddleware
}
