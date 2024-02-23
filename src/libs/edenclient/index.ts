import type { Elysia } from 'elysia'
import { EdenClient } from './types'
import { catchError, config, of } from 'rxjs'
import { EdenFetchError } from './error'
import { HttpClient, HttpErrorResponse, HttpHeaders, HttpParams } from '@angular/common/http'
import { assertInInjectionContext, inject } from '@angular/core'
import { composePath } from './utils/other'

// @ts-ignore
const isServer = typeof FileList === 'undefined'

const isFile = (v: any) => {
    // @ts-ignore
    if (isServer) {
        return v instanceof Blob
    } else {
        // @ts-ignore
        return v instanceof FileList || v instanceof File
    }
}

// FormData is 1 level deep
const hasFile = (obj: Record<string, any>) => {
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

// @ts-ignore
const createNewFile = (v: File) =>
    isServer
        ? v
        : new Promise<File>((resolve) => {
            // @ts-ignore
            const reader = new FileReader()

            reader.onload = () => {
                const file = new File([reader.result!], v.name, {
                    lastModified: v.lastModified,
                    type: v.type
                })
                resolve(file)
            }

            reader.readAsArrayBuffer(v)
        })


const createProxy = (
    domain: string,
    path = '',
    httpClient: HttpClient
): Record<string, any> => {
    return new Proxy(() => { }, {
        get(_, key: string) {
            return createProxy(domain, `${path}/${key}`, httpClient);
        },
        apply(_, __, [initialBody = {}, options = {}]: [
            {
                $query?: Record<string, string>,
                $headers?: Record<string, string>,
            },
            any
        ]) {
            const { $query, $headers, ...body } = initialBody;
            const i = path.lastIndexOf('/'),
                method = path.slice(i + 1),
                endpoint = composePath(
                    domain,
                    i === -1 ? '/' : path.slice(0, i),
                    Object.assign(options.query ?? {}, $query)
                )

            const httpOptions = {
                headers: new HttpHeaders($headers),
            };

            const errorHandler = catchError((error: HttpErrorResponse) => {
                return of(new EdenFetchError(error.status, error.message))
            })

            switch (method) {
                case 'get':
                    return httpClient.get(endpoint, httpOptions as any).pipe(errorHandler);
                case 'post':
                    return httpClient.post(endpoint, body, httpOptions as any).pipe(errorHandler);
                case 'put':
                    return httpClient.put(endpoint, body, httpOptions as any).pipe(errorHandler);
                case 'delete':
                    return httpClient.delete(endpoint, httpOptions as any).pipe(errorHandler);
                default:
                    throw new Error(`Method ${method.toUpperCase()} is not supported`);
            }
        }
    });
};

export const edenClient = <App extends Elysia<any, any, any, any, any, any>>(
    domain: string,
): EdenClient.Create<App> => {
    assertInInjectionContext(() => `edenClient can only be called inside of the constuctor context`)
    const httpClient = inject(HttpClient)
    return new Proxy(
        {},
        {
            get(target, key) {
                return createProxy(domain, key as string, httpClient)
            }
        }
    ) as any
}