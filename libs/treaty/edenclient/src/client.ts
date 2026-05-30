/**
 * @module @treaty/httpclient/client
 *
 * The typed HTTP client entry point. Provides {@link createClient} — a fully type-safe Proxy
 * built from an Elysia `App` schema — plus a promise-flavoured variant ({@link createPromiseClient})
 * for callers that prefer `await` over RxJS.
 *
 * The observable client is identical in behaviour to the legacy `edenClient` export (back-compat is
 * preserved by re-exporting from `./index`); `createClient` is simply the renamed, forward-looking
 * surface that mirrors Eden's `treaty`/`edenTreaty` naming and pairs with the `resources` entry point.
 *
 * @example
 * ```ts
 * import { createClient } from '@treaty/httpclient/client';
 *
 * const client = createClient<App>('http://localhost:3000');
 * client.users.get().subscribe(res => console.log(res.data));
 * ```
 */

import type { Elysia } from 'elysia'
import { firstValueFrom, type Observable } from 'rxjs'
import { EdenClient } from './lib/types'
import { createRootProxy, throwEdenErrors } from './lib/proxy'
import type { InferResponse } from './lib/types'
import {
	HttpClient,
} from '@angular/common/http'
import { assertInInjectionContext, inject } from '@angular/core'

/**
 * Type of the fully-typed observable client for an Elysia `App`.
 * Identical to `EdenClient.Create<App>`; exported as a friendlier alias.
 */
export type Client<App extends Elysia<any, any, any, any, any, any>> =
	EdenClient.Create<App>

/**
 * Creates a fully type-safe HTTP client for an Elysia-based API.
 *
 * This is the canonical, forward-looking constructor. It returns the same typed Proxy as the
 * legacy {@link edenClient}, preserving `EdenClient.Create<App>` typing. Each leaf call returns an
 * RxJS `Observable` of the route's response.
 *
 * Must be called within an Angular injection context (it injects `HttpClient`).
 *
 * @typeParam App The Elysia application type describing the API schema.
 * @param domain The base URL of the API.
 * @returns A typed client Proxy.
 */
export const createClient = <App extends Elysia<any, any, any, any, any, any>>(
	domain: string
): Client<App> => {
	assertInInjectionContext(
		() => `createClient can only be called inside of an injection context`
	)
	const httpClient = inject(HttpClient)
	return createRootProxy(domain, httpClient) as Client<App>
}

/**
 * Converts a single observable client call into a `Promise` of its (typed) response.
 *
 * Unlike the observable form (which emits an `EdenFetchError` *value* on failure), the promise form
 * rejects with the `EdenFetchError`, so it can be `try/catch`-ed and so the promise-based
 * {@link edenResource} surfaces failures through the resource `error()` signal.
 *
 * @example
 * ```ts
 * const res = await toPromise(client.users.get());
 * ```
 */
export const toPromise = <T>(call: Observable<T>): Promise<T> =>
	firstValueFrom(call.pipe(throwEdenErrors<T>()))

/**
 * Maps a typed observable client to a promise-returning client of the same shape, recursively.
 * Leaf calls return `Promise<DetailedResponse<...>>` instead of `Observable<DetailedResponse<...>>`.
 */
export type PromiseClient<T> = T extends (
	...args: infer A
) => Observable<infer R>
	? (...args: A) => Promise<R>
	: T extends object
	? { [K in keyof T]: PromiseClient<T[K]> }
	: T

/**
 * Wraps an existing typed observable client so every leaf call returns a `Promise` instead of an
 * `Observable`. Pairs with the promise-form {@link edenResource} / Angular's `resource()`.
 *
 * @param client A client produced by {@link createClient} / {@link edenClient}.
 * @returns A structurally identical client whose calls resolve promises.
 */
export const asPromiseClient = <App extends Elysia<any, any, any, any, any, any>>(
	client: Client<App>
): PromiseClient<Client<App>> => wrapPromise(client) as PromiseClient<Client<App>>

const wrapPromise = (target: any): any =>
	new Proxy(target, {
		get(t, key) {
			return wrapPromise(t[key as any])
		},
		apply(t, thisArg, args) {
			const result: any = Reflect.apply(t as any, thisArg, args)
			// Leaf calls return observables; wrap them into promises.
			if (result && typeof result.subscribe === 'function') {
				return toPromise(result as Observable<unknown>)
			}
			return wrapPromise(result)
		},
	})

/**
 * Creates a promise-flavoured type-safe client for an Elysia-based API.
 * Convenience wrapper over `asPromiseClient(createClient<App>(domain))`.
 *
 * @typeParam App The Elysia application type describing the API schema.
 * @param domain The base URL of the API.
 */
export const createPromiseClient = <
	App extends Elysia<any, any, any, any, any, any>
>(
	domain: string
): PromiseClient<Client<App>> => asPromiseClient(createClient<App>(domain))

export { EdenClient } from './lib/types'
export type { InferResponse, InferResponseFromThunk } from './lib/types'
export { EdenFetchError } from './lib/error'

/**
 * Back-compat re-export so `@treaty/httpclient/client` also surfaces the original `edenClient`.
 */
export { edenClient } from './index'
