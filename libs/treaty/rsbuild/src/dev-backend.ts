/**
 * @module
 *
 * Dev-server BACKEND for Treaty server functions, wired into Rsbuild's dev server.
 *
 * The compiler lifts every server-fn BODY out of the client bundle and leaves the
 * client a tiny RPC stub that `fetch`es `/__server/<name>` (see
 * {@link ./server-chunks.ts}). In a one-shot `rsbuild build` the body is emitted as
 * its own `<id>.server.js` chunk plus a `treaty-server-fns.json` manifest. But the
 * Rsbuild DEV server served nothing under `/__server/*`, so the stub's `fetch` 404'd
 * and server code never RAN — the gap this module closes, at PARITY with
 * `@treaty/vite`'s dev backend.
 *
 * The request handling itself is NOT re-implemented: it reuses the EXACT shared,
 * bundler-agnostic connect middleware from `@treaty/vite`
 * ({@link createServerFnMiddleware} + {@link parseServerFnRoute} + {@link decodeArgs}).
 * That middleware parses `/__server/<name>`, decodes the POSTed args, loads the
 * ORIGINAL server module through a {@link DevBackendServer.ssrLoadModule} adapter to
 * run the real body in the Node dev process, and replies (JSON, or SSE for an
 * async-generator). Only the per-bundler registration differs: Vite hands the
 * middleware its own connect stack + `ssrLoadModule`; here we register it on Rsbuild's
 * dev middleware chain (`dev.setupMiddlewares`) with a Node-loader `ssrLoadModule`.
 *
 * CLIENT PRIVACY is preserved exactly as in Vite: the body is loaded server-side only
 * and never enters the client module graph (the client follows the binding to the RPC
 * stub).
 */

import { pathToFileURL } from 'node:url'
import {
	createServerFnMiddleware,
	type DevBackendServer,
	type DevConnectMiddleware,
	type DevReq,
	type DevRes,
	type DevServerFn,
} from '@treaty/vite'
import type { RsbuildConfig } from '@rsbuild/core'

export type { DevServerFn } from '@treaty/vite'
export { parseServerFnRoute, decodeArgs, SERVER_ROUTE_PREFIX } from '@treaty/vite'

/**
 * SSR-load the original server module in the Node dev process so its real body runs.
 *
 * Rsbuild does not expose a Vite-style SSR module runner to plugin middleware, so the
 * faithful dev-process loader is a plain dynamic `import()` of the on-disk module by
 * `file://` URL. Node (>=22, native) strips TypeScript types from a `.ts` server module
 * on import, so a pure `'use server'` `.ts`/`.mjs`/`.js` module (the common server-fn
 * shape — a colocated or sibling server module) executes its genuine body directly.
 *
 * A cache-busting `?t=<now>` query is appended so an edited server module is re-loaded
 * in dev rather than served from Node's module cache (mirroring how Vite's
 * `ssrLoadModule` re-evaluates a changed module).
 *
 * A module Node cannot natively import (e.g. a `.treaty`/`.tsx`/`.tjsx` authoring file
 * with a colocated `server { … }` block, whose body the bundler would have to transpile)
 * rejects here; the shared middleware turns that into a clean `500` with the loader
 * error rather than a hang. That authoring-colocated SSR path is the peer-gated boundary
 * — running it needs a bundler SSR runner Rsbuild does not surface to middleware.
 */
async function nodeSsrLoadModule(id: string): Promise<Record<string, unknown>> {
	const url = `${pathToFileURL(id).href}?t=${Date.now()}`
	const mod = (await import(url)) as Record<string, unknown>
	return mod
}

/**
 * The Node-loader {@link DevBackendServer} the shared middleware runs against. Its
 * `middlewares.use` is a no-op stand-in: we do not let the shared factory push onto a
 * connect stack itself (Rsbuild owns the chain) — we take the middleware the factory
 * RETURNS and register THAT on Rsbuild's `dev.setupMiddlewares`. Only `ssrLoadModule`
 * is load-bearing here.
 */
const NODE_BACKEND: DevBackendServer = {
	middlewares: { use() {} },
	ssrLoadModule: nodeSsrLoadModule,
}

/**
 * The Rsbuild dev-middleware stack handed to a `dev.setupMiddlewares` entry. It is the
 * connect/`http` middleware array Rsbuild builds its dev server from; we `unshift` the
 * server-fn middleware so it claims a `/__server/<name>` request before Rsbuild's SPA
 * history-fallback middleware would answer it with `index.html`.
 */
interface RsbuildMiddlewareStack {
	unshift(...handlers: DevConnectMiddleware[]): void
	push(...handlers: DevConnectMiddleware[]): void
}

/** A single `dev.setupMiddlewares` entry: `(middlewares, server) => void`. */
type SetupMiddlewaresFn = (middlewares: RsbuildMiddlewareStack, server: unknown) => void

/**
 * The slice of `RsbuildConfig` this module mutates: the dev-server middleware hook.
 * Declared as a local widening of the structural {@link RsbuildConfig} so we can set
 * `dev.setupMiddlewares` without the full `@rsbuild/core` types.
 */
type RsbuildConfigWithDev = RsbuildConfig & {
	dev?: {
		setupMiddlewares?: SetupMiddlewaresFn[]
		[key: string]: unknown
	}
}

/**
 * Build the Rsbuild server-fn dev backend over a server-fn `registry`.
 *
 * Returns a `modifyRsbuildConfig` modifier that appends a `dev.setupMiddlewares` entry,
 * which in turn `unshift`s the SHARED `@treaty/vite` server-fn middleware (bound to the
 * Node-loader {@link DevBackendServer}) onto Rsbuild's dev middleware chain. The
 * `registry` is populated lazily by the plugin's `transform` as authoring files
 * declaring server fns are compiled, and read at request time by the middleware — so a
 * fn the dev session has not yet compiled simply 404s until its module is transformed,
 * exactly as in Vite.
 */
export function devBackendConfigModifier(
	registry: ReadonlyMap<string, DevServerFn>
): (config: RsbuildConfig) => void {
	const middleware = createServerFnMiddleware(NODE_BACKEND, registry)
	return (config: RsbuildConfig): void => {
		const dev = ((config as RsbuildConfigWithDev).dev ??= {})
		const stack = (dev.setupMiddlewares ??= [])
		stack.push((middlewares: RsbuildMiddlewareStack) => {
			// Unshift so `/__server/<name>` is claimed before Rsbuild's SPA fallback.
			middlewares.unshift(middleware)
		})
	}
}

export type { DevReq, DevRes, DevConnectMiddleware, DevBackendServer }
export { createServerFnMiddleware }
