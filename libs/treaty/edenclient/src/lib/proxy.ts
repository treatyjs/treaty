/**
 * @module
 *
 * Internal proxy machinery shared by the {@link edenClient} / {@link createClient}
 * entry points. The proxy turns property access + invocation on a typed client object
 * into Angular `HttpClient` calls against an Elysia-style API, returning RxJS observables.
 *
 * This module is intentionally framework-internal: consumers should use `createClient`
 * (from `@treaty/httpclient/client`) or `edenClient` (from `@treaty/httpclient`).
 */

import { catchError, of, map, type Observable } from 'rxjs'
import { EdenFetchError } from './error'
import {
	HttpClient,
	HttpErrorResponse,
	HttpHeaders,
} from '@angular/common/http'
import { composePath } from './utils/other'

// @ts-ignore - `FileList` is undefined in non-browser (server / test) environments.
const isServer = typeof FileList === 'undefined'

/**
 * Determines if the provided value is a File or FileList, facilitating the handling of file inputs.
 * @param v The value to check.
 * @returns A boolean indicating whether the value is a File or FileList.
 */
export const isFile = (v: any) => {
	// @ts-ignore
	if (isServer) {
		return v instanceof Blob
	} else {
		// @ts-ignore
		return v instanceof FileList || v instanceof File
	}
}

/**
 * Checks if the given object contains a File or FileList, aiding in detecting file inputs within objects.
 * FormData is 1 level deep
 * @param obj The object to check.
 * @returns A boolean indicating if the object contains a File or FileList.
 */
export const hasFile = (obj: Record<string, any>) => {
	if (!obj) return false

	for (const key in obj) {
		if (isFile(obj[key])) return true
		else if (
			Array.isArray(obj[key]) &&
			(obj[key] as unknown[]).find((x) => isFile(x))
		)
			return true
	}

	return false
}

/**
 * Creates a new File instance from a given File, useful in environments where FileList is not defined.
 * @param v The File to convert.
 * @returns A Promise resolving to a File.
 */
// @ts-ignore
export const createNewFile = (v: File) =>
	isServer
		? v
		: new Promise<File>((resolve) => {
				// @ts-ignore
				const reader = new FileReader()

				reader.onload = () => {
					const file = new File([reader.result!], v.name, {
						lastModified: v.lastModified,
						type: v.type,
					})
					resolve(file)
				}

				reader.readAsArrayBuffer(v)
		  })

/**
 * Creates a proxy for making HTTP requests to a specified domain, utilizing Angular's HttpClient.
 *
 * Each property access appends a path segment; the final segment is interpreted as the HTTP method
 * (`get` | `post` | `put` | `delete`). Invoking the resulting function performs the request and
 * returns an RxJS `Observable` of the response body.
 *
 * @param domain The base domain for the API.
 * @param path Initial path segment for the API endpoint.
 * @param httpClient An instance of Angular's HttpClient.
 * @returns A proxy object for making API requests.
 */
export const createProxy = (
	domain: string,
	path = '',
	httpClient: HttpClient
): Record<string, any> => {
	return new Proxy(() => {}, {
		get(_, key: string) {
			return createProxy(domain, `${path}/${key}`, httpClient)
		},
		apply(
			_,
			__,
			[initialBody = {}, options = {}]: [
				{
					$query?: Record<string, string>
					$headers?: Record<string, string>
				},
				any
			]
		) {
			const { $query, $headers, ...body } = initialBody
			const i = path.lastIndexOf('/'),
				method = path.slice(i + 1),
				endpoint = composePath(
					domain,
					i === -1 ? '/' : path.slice(0, i),
					Object.assign(options.query ?? {}, $query)
				)

			const httpOptions = {
				headers: new HttpHeaders($headers),
			}

			const errorHandler = catchError((error: HttpErrorResponse) => {
				return of(new EdenFetchError(error.status, error.message))
			})

			switch (method) {
				case 'get':
					return httpClient
						.get(endpoint, httpOptions as any)
						.pipe(errorHandler)
				case 'post':
					return httpClient
						.post(endpoint, body, httpOptions as any)
						.pipe(errorHandler)
				case 'put':
					return httpClient
						.put(endpoint, body, httpOptions as any)
						.pipe(errorHandler)
				case 'delete':
					return httpClient
						.delete(endpoint, httpOptions as any)
						.pipe(errorHandler)
				default:
					throw new Error(`Method ${method.toUpperCase()} is not supported`)
			}
		},
	})
}

/**
 * Builds the root proxy object for a client bound to `domain` and `httpClient`.
 * The first property access (e.g. `client.users`) starts a fresh path-building proxy.
 */
export const createRootProxy = (
	domain: string,
	httpClient: HttpClient
): Record<string, any> =>
	new Proxy(
		{},
		{
			get(_target, key) {
				return createProxy(domain, key as string, httpClient)
			},
		}
	)

/**
 * Re-throws {@link EdenFetchError} so promise/resource consumers can react to errors via
 * rejection / the resource `error()` signal instead of receiving an error object as a value.
 *
 * The observable client swallows HTTP errors into an `EdenFetchError` *value*. For the promise
 * and resource ergonomics we want errors to surface as actual rejections, so this operator
 * converts an emitted `EdenFetchError` back into a thrown error.
 */
export const throwEdenErrors = <T>(): (source: Observable<T>) => Observable<T> => {
	return (source: Observable<T>) =>
		source.pipe(
			map((value) => {
				if (value instanceof EdenFetchError) throw value
				return value
			})
		)
}
