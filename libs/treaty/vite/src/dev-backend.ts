/**
 * @module
 *
 * Dev-server BACKEND for Treaty server functions.
 *
 * The compiler lifts every server-fn BODY out of the client bundle and leaves
 * the client a tiny RPC stub that `fetch`es `/__server/<name>` (see
 * {@link ./server-chunks.ts}). In a one-shot `vite build` the lifted body is
 * emitted as its own `<id>.server.js` chunk plus a `treaty-server-fns.json`
 * manifest for a production host to serve. But in DEV nothing served
 * `/__server/*`, so the stub's `fetch` 404'd and server code never RAN.
 *
 * This module is the dev backend: a connect middleware that, for a request to
 * `/__server/<name>`, loads the ORIGINAL server module through Vite's SSR module
 * runner (`server.ssrLoadModule`) — which executes the real TypeScript body in
 * the Node dev process — calls the named export with the POSTed arguments, and
 * returns the result. An async-generator (streaming) export is streamed to the
 * client as Server-Sent Events; a plain async/sync export returns its awaited
 * value as JSON.
 *
 * CLIENT PRIVACY is preserved: the body is loaded SSR-side only. It never enters
 * the client module graph (the client follows the binding to the RPC stub) and
 * is never written to disk in dev. This module reads the original `.server.ts`
 * via SSR, so the genuine server code runs without ever being shipped.
 */

/** Server route prefix every backend registers a fn under (`/__server/<name>`). */
export const SERVER_ROUTE_PREFIX = '/__server/'

/**
 * A registered dev server fn: the absolute/relative module id whose SSR-loaded
 * exports carry the real body, keyed in the registry by the fn's export name.
 * The `exportName` is also the route segment the client stub fetches.
 */
export interface DevServerFn {
	/** The author's exported name (the `/__server/<exportName>` route segment). */
	readonly exportName: string
	/** The module id to `ssrLoadModule`, whose `[exportName]` export is the body. */
	readonly moduleId: string
}

/**
 * The minimal Vite dev-server surface this middleware needs: the connect
 * middleware stack and the SSR module loader. Declared structurally so the
 * plugin keeps typechecking without a value-level Vite dependency.
 */
export interface DevBackendServer {
	readonly middlewares: { use(fn: DevConnectMiddleware): void }
	ssrLoadModule(id: string): Promise<Record<string, unknown>>
	ssrFixStacktrace?(e: unknown): void
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
export type DevConnectMiddleware = (req: DevReq, res: DevRes, next: (err?: unknown) => void) => void

/** Parse the `/__server/<name>` route segment out of a request url, or `null`. */
export function parseServerFnRoute(url: string | undefined): string | null {
	if (typeof url !== 'string') return null
	// Strip the query/hash; the route is the path component only.
	const path = url.replace(/[?#].*$/, '')
	if (!path.startsWith(SERVER_ROUTE_PREFIX)) return null
	const name = path.slice(SERVER_ROUTE_PREFIX.length)
	// A bare identifier segment only — no nested path, so `/__server/a/b` is rejected.
	if (name.length === 0 || !/^[A-Za-z_$][\w$]*$/.test(name)) return null
	return name
}

/** Read a connect `req` body to a string (UTF-8), resolving on `end`. */
function readBody(req: DevReq): Promise<string> {
	return new Promise((resolve, reject) => {
		let data = ''
		req.on('data', (chunk) => {
			data += typeof chunk === 'string' ? chunk : chunk.toString('utf8')
		})
		req.on('end', () => resolve(data))
		req.on('error', (err) => reject(err))
	})
}

/**
 * Decode the POSTed argument list. The client stub sends
 * `JSON.stringify(args.length === 1 ? args[0] : args)`, so a single-arg call
 * carries the bare value and a multi-arg call carries the array. We normalize
 * back to a positional argument array for `fn(...args)`:
 *   * an empty body  -> `[]`
 *   * a JSON array   -> spread as the positional args
 *   * any other JSON -> a single positional arg
 */
export function decodeArgs(body: string): unknown[] {
	const trimmed = body.trim()
	if (trimmed.length === 0) return []
	const parsed: unknown = JSON.parse(trimmed)
	return Array.isArray(parsed) ? parsed : [parsed]
}

/** Whether `value` is an async iterable (the async-generator streaming shape). */
function isAsyncIterable(value: unknown): value is AsyncIterable<unknown> {
	return (
		value != null &&
		typeof (value as { [Symbol.asyncIterator]?: unknown })[Symbol.asyncIterator] === 'function'
	)
}

/**
 * Whether `value` is a SYNC GENERATOR object (the result of calling a
 * `function*`), as opposed to an ordinary iterable collection. A plain `Array`,
 * `Set`, `Map`, typed array, or string is ALSO `Symbol.iterator`-iterable, but an
 * API server fn that returns `Todo[]` must be sent as a single JSON value — not
 * streamed frame-by-frame. So we stream a sync iterable ONLY when it is a true
 * generator (its `Symbol.toStringTag` is `'Generator'`), which `function*`
 * produces and a collection never does.
 */
function isSyncGenerator(value: unknown): value is Iterable<unknown> {
	if (value == null || typeof value !== 'object') return false
	const tag = (value as { [Symbol.toStringTag]?: unknown })[Symbol.toStringTag]
	return (
		tag === 'Generator' &&
		typeof (value as { [Symbol.iterator]?: unknown })[Symbol.iterator] === 'function'
	)
}

/**
 * Stream an (async) iterable to the client as Server-Sent Events: one `data:`
 * frame per yielded value (JSON-encoded), then a terminating `event: end` frame.
 * This is the dev transport for an `async function*` server fn — the client's
 * `EventSource`/stream binding receives one event per `yield`.
 */
async function streamSse(res: DevRes, iterable: AsyncIterable<unknown> | Iterable<unknown>): Promise<void> {
	res.statusCode = 200
	res.setHeader('content-type', 'text/event-stream; charset=utf-8')
	res.setHeader('cache-control', 'no-cache, no-transform')
	res.setHeader('connection', 'keep-alive')
	for await (const value of iterable as AsyncIterable<unknown>) {
		res.write(`data: ${JSON.stringify(value)}\n\n`)
	}
	res.write('event: end\ndata: {}\n\n')
	res.end()
}

/** Send a JSON value as a `200 application/json` response. */
function sendJson(res: DevRes, value: unknown): void {
	res.statusCode = 200
	res.setHeader('content-type', 'application/json; charset=utf-8')
	// `undefined` is not valid JSON; normalize a void return to `null`.
	res.end(JSON.stringify(value === undefined ? null : value))
}

/** Send a plain-text error with the given status. */
function sendError(res: DevRes, status: number, message: string): void {
	res.statusCode = status
	res.setHeader('content-type', 'text/plain; charset=utf-8')
	res.end(message)
}

/**
 * Build the dev backend middleware over a registry of server fns.
 *
 * `registry` maps an export name to the module id whose SSR-loaded export is the
 * real body; it is populated by the plugin's `transform` as it discovers server
 * fns (so only fns the app actually declares are routable). The middleware:
 *
 *   1. Matches `/__server/<name>` (any method; the client stub POSTs).
 *   2. Looks `<name>` up in the registry; a miss falls through to `next()` (so a
 *      non-server request is untouched and a genuinely unknown fn 404s via Vite).
 *   3. `ssrLoadModule`s the fn's module — running the REAL TypeScript body in the
 *      Node dev process — and reads its `[name]` export.
 *   4. Decodes the POSTed args, invokes the fn, and replies: a streaming
 *      (async-iterable) result is sent as SSE; any other result is awaited and
 *      sent as JSON.
 *
 * Errors are reported as `500` with the message (and the stack is fixed up via
 * `ssrFixStacktrace` when available) so a throwing server fn surfaces in dev
 * rather than hanging the request.
 */
export function createServerFnMiddleware(
	server: DevBackendServer,
	registry: ReadonlyMap<string, DevServerFn>
): DevConnectMiddleware {
	return (req, res, next) => {
		const name = parseServerFnRoute(req.url)
		if (name === null) {
			next()
			return
		}
		const entry = registry.get(name)
		if (entry === undefined) {
			// A `/__server/*` path with no registered fn: 404 explicitly rather than
			// falling through to Vite's SPA/HTML fallback (which would 200 with HTML
			// and confuse the client stub's JSON parse).
			sendError(res, 404, `treaty: no server fn '${name}' is registered`)
			return
		}

		void (async () => {
			try {
				const body = await readBody(req)
				const args = decodeArgs(body)
				const mod = await server.ssrLoadModule(entry.moduleId)
				const fn = mod[entry.exportName]
				if (typeof fn !== 'function') {
					sendError(res, 500, `treaty: server fn '${name}' is not an exported function`)
					return
				}
				const result = (fn as (...a: unknown[]) => unknown)(...args)
				const awaited = result instanceof Promise ? await result : result
				if (isAsyncIterable(awaited) || isSyncGenerator(awaited)) {
					await streamSse(res, awaited)
					return
				}
				sendJson(res, awaited)
			} catch (err) {
				if (typeof server.ssrFixStacktrace === 'function') server.ssrFixStacktrace(err)
				const message = err instanceof Error ? err.stack ?? err.message : String(err)
				sendError(res, 500, `treaty: server fn '${name}' threw:\n${message}`)
			}
		})()
	}
}
