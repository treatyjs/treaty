/**
 * @module
 *
 * This module implements a client for the Elysia framework, providing utilities for creating observable-based API clients with TypeScript. It includes type-safe utilities for handling files, making HTTP requests, and processing responses. It leverages RxJS for observables and integrates with Angular's HttpClient for HTTP requests. Key features include type-safe route definition, detailed error handling, and support for file uploads.
 *
 * Example usage:
 * ```ts
 * import { edenClient } from "@treaty/httpclient@0.0";
 * const client = edenClient<MyAppSchema>('http://api.example.com');
 * ```
 *
 * New, forward-looking entry points are also available:
 * - `@treaty/httpclient/client`    — {@link createClient} (typed HTTP client) + promise helpers.
 * - `@treaty/httpclient/resources` — `edenResource` / `edenHttpResource` signal resources.
 *
 * For convenience their public surface is also re-exported from this barrel.
 */

import type { Elysia } from 'elysia'
import { EdenClient } from './lib/types'
import { assertInInjectionContext, inject } from '@angular/core'
import { HttpClient } from '@angular/common/http'
import { createRootProxy } from './lib/proxy'

/**
 * Initializes an EdenClient for interacting with an Elysia-based API.
 *
 * Retained for backwards compatibility; {@link createClient} (from `@treaty/httpclient/client`)
 * is the preferred forward-looking name and returns the identical typed Proxy.
 *
 * @param domain The base URL of the API.
 * @returns An instance of EdenClient configured for the specified Elysia application.
 */
export const edenClient = <App extends Elysia<any, any, any, any, any, any>>(
	domain: string
): EdenClient.Create<App> => {
	assertInInjectionContext(
		() => `edenClient can only be called inside of the constuctor context`
	)
	const httpClient = inject(HttpClient)
	return createRootProxy(domain, httpClient) as any
}

// ---------------------------------------------------------------------------
// Re-exports: keep the barrel a one-stop import while the dedicated entry
// points (`/client`, `/resources`) provide tree-shakable, focused surfaces.
// ---------------------------------------------------------------------------

export { EdenClient } from './lib/types'
export type {
	InferResponse,
	InferResponseFromThunk,
	Awaited2,
} from './lib/types'
export { EdenFetchError } from './lib/error'

export {
	createClient,
	createPromiseClient,
	asPromiseClient,
	toPromise,
} from './client'
export type { Client, PromiseClient } from './client'

export {
	edenResource,
	edenHttpResource,
	edenPromiseResource,
} from './resources'
export type { EdenResourceOptions } from './resources'
