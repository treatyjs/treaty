/**
 * @module @treaty/httpclient/resources
 *
 * Signal-based resources for the typed Eden client, mirroring Angular's `resource()` /
 * `httpResource()` API. These helpers turn a typed client call into an Angular
 * `Resource<T>` exposing `value()`, `status()`, `error()`, `isLoading()`, `hasValue()` and
 * `reload()` — where `T` is **inferred** end-to-end from the Elysia route's response type.
 *
 * Two flavours are provided, paralleling Angular's own split:
 *
 * - {@link edenResource} / {@link edenHttpResource} — observable-driven, backed by `rxResource`.
 *   This mirrors `httpResource()` (the call performs an HTTP request and yields an Observable).
 * - {@link edenPromiseResource} — promise-driven, backed by `resource`. This mirrors the generic
 *   `resource()` for callers using {@link createPromiseClient} / `asPromiseClient`.
 *
 * @example
 * ```ts
 * import { createClient } from '@treaty/httpclient/client';
 * import { edenHttpResource } from '@treaty/httpclient/resources';
 *
 * const client = createClient<App>('http://localhost:3000');
 *
 * // `users.value()` is typed as the route's 200 response, fully inferred.
 * const users = edenHttpResource(() => client.users.get());
 * users.value();   // Signal<ResponseType | undefined>
 * users.status();  // Signal<ResourceStatus>
 * users.reload();
 * ```
 */

import {
	type Injector,
	type ResourceRef,
	type Signal,
} from '@angular/core'
import { resource } from '@angular/core'
import { rxResource } from '@angular/core/rxjs-interop'
import { type Observable } from 'rxjs'
import { throwEdenErrors } from './lib/proxy'
import type { Awaited2, InferResponse } from './lib/types'

/**
 * Common reactive + injection options shared by the resource helpers. Mirrors the relevant subset
 * of Angular's `BaseResourceOptions`, plus an explicit reactive `params` source.
 *
 * @typeParam R The reactive request/params type. Whenever `params()` changes the resource reloads.
 */
export interface EdenResourceOptions<T, R = unknown> {
	/**
	 * Reactive request source. When this changes, the loader is re-invoked. The latest value is
	 * passed to the call factory so the request can depend on it.
	 */
	params?: () => R
	/** Default value while the resource is loading / before first resolution. */
	defaultValue?: T
	/** Overrides the injector used by the underlying Angular resource. */
	injector?: Injector
	/** A debug name surfaced in Angular DevTools. */
	debugName?: string
}

/** {@link EdenResourceOptions} with a required `defaultValue`, narrowing the resource to `T`. */
export interface EdenResourceOptionsWithDefault<T, R = unknown>
	extends EdenResourceOptions<T, R> {
	defaultValue: T
}

/**
 * Creates a signal-based resource from an **observable** typed client call, mirroring
 * Angular's `httpResource()`. Backed by `rxResource` under the hood.
 *
 * The response type `T` is inferred from the call's return type (the Elysia route's `200`
 * response), giving end-to-end type safety with zero manual annotation.
 *
 * @param call A factory returning the observable client call to execute, e.g.
 *             `() => client.users.get()`. Re-evaluated whenever `options.params` changes.
 * @param options Reactive `params`, `defaultValue`, `injector`, `debugName`.
 * @returns An Angular `ResourceRef<T | undefined>` (or `ResourceRef<T>` when `defaultValue` set).
 */
export function edenHttpResource<C extends Observable<any>, R = unknown>(
	call: (params: R) => C,
	options: EdenResourceOptionsWithDefault<InferResponse<C>, R>
): ResourceRef<InferResponse<C>>
export function edenHttpResource<C extends Observable<any>, R = unknown>(
	call: (params: R) => C,
	options?: EdenResourceOptions<InferResponse<C>, R>
): ResourceRef<InferResponse<C> | undefined>
export function edenHttpResource<C extends Observable<any>, R = unknown>(
	call: (params: R) => C,
	options?: EdenResourceOptions<InferResponse<C>, R>
): ResourceRef<InferResponse<C> | undefined> {
	type T = InferResponse<C>
	return rxResource<T, R>({
		// A `null` sentinel (rather than `undefined`) ensures the resource loads even when no
		// reactive `params` are supplied — Angular skips loading only for an `undefined` request.
		params: options?.params ?? (() => null as R),
		defaultValue: options?.defaultValue as any,
		injector: options?.injector,
		stream: ({ params }: { params: R }) =>
			(call(params) as Observable<any>).pipe(throwEdenErrors<T>()),
	}) as ResourceRef<T | undefined>
}

/**
 * Alias of {@link edenHttpResource} that reads ergonomically for the common case
 * `edenResource(() => client.users.get(...))`. Identical behaviour and typing.
 */
export const edenResource = edenHttpResource

/**
 * Creates a signal-based resource from a **promise** typed client call (see
 * {@link createPromiseClient}/`asPromiseClient`), mirroring Angular's generic `resource()`.
 * Backed by `resource` under the hood.
 *
 * @param call A factory returning the promise client call, e.g. `() => promiseClient.users.get()`.
 * @param options Reactive `params`, `defaultValue`, `injector`, `debugName`.
 * @returns An Angular `ResourceRef<T | undefined>` (or `ResourceRef<T>` when `defaultValue` set).
 */
export function edenPromiseResource<C extends Promise<any>, R = unknown>(
	call: (params: R) => C,
	options: EdenResourceOptionsWithDefault<Awaited2<C>, R>
): ResourceRef<Awaited2<C>>
export function edenPromiseResource<C extends Promise<any>, R = unknown>(
	call: (params: R) => C,
	options?: EdenResourceOptions<Awaited2<C>, R>
): ResourceRef<Awaited2<C> | undefined>
export function edenPromiseResource<C extends Promise<any>, R = unknown>(
	call: (params: R) => C,
	options?: EdenResourceOptions<Awaited2<C>, R>
): ResourceRef<Awaited2<C> | undefined> {
	type T = Awaited2<C>
	return resource<T, R>({
		// See `edenHttpResource`: `null` sentinel keeps the resource loading without explicit params.
		params: options?.params ?? (() => null as R),
		defaultValue: options?.defaultValue as any,
		injector: options?.injector,
		debugName: options?.debugName,
		loader: ({ params }: { params: R }) => call(params) as Promise<T>,
	}) as ResourceRef<T | undefined>
}

export type { ResourceRef, Signal }
export type { InferResponse } from './lib/types'
