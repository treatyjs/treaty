/**
 * @module
 *
 * Concrete {@link DeployTarget} implementations.
 *
 * {@link FsDeployTarget} is the smallest *real* target — it writes a
 * {@link DeployArtifact}'s files to a local directory and serves them under a
 * configurable base url. It is the self-hosted backend.
 *
 * {@link HttpDeployTarget} is the concrete cloud / object-store target: it uploads
 * each artifact file with an HTTP `PUT` to `<baseUrl>/<deployPath>` and reports
 * that same url from `urlFor`. This is the shape every commodity object store (S3,
 * GCS, R2, Azure Blob) and most static-host upload APIs expose — a versioned key
 * is a `PUT`, the public url is the key under the bucket/CDN host — so a real cloud
 * deploy is this class with the bucket's base url (and, where needed, an `Authorization`
 * header) and no SDK required. {@link deploy} drives it unchanged.
 *
 * A target deliberately knows nothing about modules, versions, or manifests — it
 * only puts bytes at a path and reports the url for a path. All the federation
 * semantics live in {@link assembleDeployArtifact} (which produces the versioned
 * paths) and {@link deploy} (which resolves urls + repoints the manifest).
 */

import { mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { dirname, join, posix, sep } from 'node:path'
import {
	parseDeploymentManifest,
	serializeDeploymentManifest,
	type DeploymentManifest,
	type DeploymentManifestStore,
} from '@treaty/federation-deploy'
import type { DeployTarget, DeployTargetContext } from './deploy.js'

/** Options for {@link FsDeployTarget}. */
export interface FsDeployTargetOptions {
	/**
	 * Root directory artifacts are written under. A deploy path
	 * `"<moduleId>/<version>/<file>"` lands at `<root>/<moduleId>/<version>/<file>`.
	 * Defaults to the current working directory.
	 */
	readonly root?: string
	/**
	 * Base url the `root` is served from, used to build each path's url as
	 * `<baseUrl>/<deployPath>`. Defaults to a `file://` url under `root`, so the
	 * target is self-contained without config.
	 */
	readonly baseUrl?: string
	/** Registered name. Defaults to `'fs'`. */
	readonly name?: string
}

/**
 * Reference {@link DeployTarget} backed by the local filesystem — the self-hosted
 * backend and the template for any cloud/static-host target.
 *
 * `upload` writes a deploy path's bytes to `<root>/<deployPath>` (creating
 * directories as needed); `urlFor` reports `<baseUrl>/<deployPath>`. Because
 * {@link assembleDeployArtifact} laid the paths out version-stamped, two versions
 * of a module never overwrite each other on disk — which is exactly what lets a
 * later rollback repoint at the prior version's still-present files.
 */
export class FsDeployTarget implements DeployTarget {
	readonly name: string
	readonly #root: string
	readonly #baseUrl: string

	constructor(options: FsDeployTargetOptions = {}) {
		this.name = options.name ?? 'fs'
		this.#root = options.root ?? process.cwd()
		this.#baseUrl = options.baseUrl ?? pathToFileUrl(this.#root)
	}

	async upload(path: string, bytes: Uint8Array, _ctx: DeployTargetContext): Promise<void> {
		const dest = join(this.#root, path.split(posix.sep).join(sep))
		await mkdir(dirname(dest), { recursive: true })
		await writeFile(dest, bytes)
	}

	urlFor(path: string, _ctx: DeployTargetContext): string {
		return `${trimTrailingSlash(this.#baseUrl)}/${trimLeadingSlash(path)}`
	}

	async remove(path: string, _ctx: DeployTargetContext): Promise<void> {
		const dest = join(this.#root, path.split(posix.sep).join(sep))
		await rm(dest, { force: true })
	}
}

/** The `fetch` shape {@link HttpDeployTarget} drives — a structural subset of the WHATWG/Node global. */
export type FetchLike = (
	input: string,
	init: {
		method: string
		headers: Record<string, string>
		body?: Uint8Array
	}
) => Promise<{ ok: boolean; status: number; statusText: string }>

/** Options for {@link HttpDeployTarget}. */
export interface HttpDeployTargetOptions {
	/**
	 * Base url every deploy path is uploaded under and served from. A deploy path
	 * `"<moduleId>/<version>/<file>"` is `PUT` to `<baseUrl>/<moduleId>/<version>/<file>`
	 * and {@link HttpDeployTarget.urlFor} reports that same url. This is the object
	 * store / bucket / CDN origin (e.g. `https://assets.example.com/app`). Required.
	 */
	readonly baseUrl: string
	/**
	 * Url the uploaded paths are *served* from, when it differs from the upload
	 * origin (e.g. you `PUT` to an object-store endpoint but the public url is a CDN
	 * host). Used only by {@link HttpDeployTarget.urlFor}; defaults to
	 * {@link HttpDeployTargetOptions.baseUrl} so a single-origin store needs no extra
	 * config.
	 */
	readonly publicBaseUrl?: string
	/**
	 * Extra request headers sent on every upload (and removal). The place an object
	 * store's auth lives — e.g. `{ Authorization: 'Bearer …' }` or a presigned token
	 * header. Tests need none because an unauthenticated PUT store is a valid target.
	 */
	readonly headers?: Readonly<Record<string, string>>
	/**
	 * The MIME type sent as `Content-Type` when a per-extension type is not known.
	 * Defaults to `application/octet-stream`.
	 */
	readonly defaultContentType?: string
	/**
	 * The `fetch` implementation to drive. Defaults to the global `fetch` (Node ≥
	 * 18), so no injection is needed in production; tests pass one pointed at an
	 * in-process server.
	 */
	readonly fetch?: FetchLike
	/** Registered name. Defaults to `'http'`. */
	readonly name?: string
}

/**
 * Concrete cloud / object-store {@link DeployTarget}: uploads each artifact file
 * with an HTTP `PUT` to `<baseUrl>/<deployPath>` and serves it from that url.
 *
 * This is the real, credential-free shape behind every commodity object store and
 * static-host upload API: a versioned key (`<moduleId>/<version>/<file>`) is the
 * `PUT` target, and the public url is that key under the bucket/CDN origin. Because
 * {@link assembleDeployArtifact} already version-stamps every path, two versions of
 * a module land at distinct keys and never overwrite each other — which is what
 * lets {@link rollback} repoint at a prior version's still-published objects without
 * re-uploading. A real S3/GCS/R2 deploy is this class with the bucket base url and,
 * where required, an `Authorization`/presigned header in {@link HttpDeployTargetOptions.headers}.
 *
 * `upload` issues `PUT <baseUrl>/<path>` with the file bytes (and a content type
 * inferred from the extension); `urlFor` reports `<publicBaseUrl ?? baseUrl>/<path>`;
 * `remove` issues `DELETE` for rollback cleanup. A non-2xx response throws so a
 * failed deploy is never silently reported as live.
 */
export class HttpDeployTarget implements DeployTarget {
	readonly name: string
	readonly #baseUrl: string
	readonly #publicBaseUrl: string
	readonly #headers: Readonly<Record<string, string>>
	readonly #defaultContentType: string
	readonly #fetch: FetchLike

	constructor(options: HttpDeployTargetOptions) {
		if (!options || typeof options.baseUrl !== 'string' || options.baseUrl.length === 0) {
			throw new TypeError('HttpDeployTarget: options.baseUrl is required')
		}
		const globalFetch = (globalThis as { fetch?: unknown }).fetch
		const fetchImpl = options.fetch ?? (globalFetch as FetchLike | undefined)
		if (typeof fetchImpl !== 'function') {
			throw new TypeError(
				'HttpDeployTarget: no fetch available — pass options.fetch (global fetch needs Node >= 18)'
			)
		}
		this.name = options.name ?? 'http'
		this.#baseUrl = trimTrailingSlash(options.baseUrl)
		this.#publicBaseUrl = trimTrailingSlash(options.publicBaseUrl ?? options.baseUrl)
		this.#headers = options.headers ?? {}
		this.#defaultContentType = options.defaultContentType ?? 'application/octet-stream'
		this.#fetch = fetchImpl
	}

	async upload(path: string, bytes: Uint8Array, _ctx: DeployTargetContext): Promise<void> {
		const url = `${this.#baseUrl}/${trimLeadingSlash(path)}`
		const res = await this.#fetch(url, {
			method: 'PUT',
			headers: {
				'Content-Type': contentTypeFor(path, this.#defaultContentType),
				'Content-Length': String(bytes.byteLength),
				...this.#headers,
			},
			body: bytes,
		})
		if (!res.ok) {
			throw new Error(`HttpDeployTarget(${this.name}): PUT ${url} failed: ${res.status} ${res.statusText}`)
		}
	}

	urlFor(path: string, _ctx: DeployTargetContext): string {
		return `${this.#publicBaseUrl}/${trimLeadingSlash(path)}`
	}

	async remove(path: string, _ctx: DeployTargetContext): Promise<void> {
		const url = `${this.#baseUrl}/${trimLeadingSlash(path)}`
		const res = await this.#fetch(url, { method: 'DELETE', headers: { ...this.#headers } })
		// Treat a missing object as already-removed (idempotent cleanup); other
		// non-2xx is a real failure.
		if (!res.ok && res.status !== 404) {
			throw new Error(`HttpDeployTarget(${this.name}): DELETE ${url} failed: ${res.status} ${res.statusText}`)
		}
	}
}

/** Options for {@link FsDeploymentStore}. */
export interface FsDeploymentStoreOptions {
	/**
	 * Path to the JSON file the deployment manifest (per-remote ledger) is persisted
	 * at. Created on first {@link FsDeploymentStore.save}; missing means "no manifest
	 * yet" on {@link FsDeploymentStore.load}.
	 */
	readonly path: string
}

/**
 * Reference {@link DeploymentManifestStore} backed by a local JSON file — the
 * self-hosted ledger store and the template for a database/object-store/config-
 * service implementation. {@link FsDeploymentStore.load} returns `undefined` when
 * the file does not exist yet (a first deploy seeds it); {@link FsDeploymentStore.save}
 * writes the canonical serialization, creating parent directories as needed.
 */
export class FsDeploymentStore implements DeploymentManifestStore {
	readonly #path: string

	constructor(options: FsDeploymentStoreOptions) {
		if (!options || typeof options.path !== 'string' || options.path.length === 0) {
			throw new TypeError('FsDeploymentStore: options.path is required')
		}
		this.#path = options.path
	}

	async load(): Promise<DeploymentManifest | undefined> {
		let json: string
		try {
			json = await readFile(this.#path, 'utf8')
		} catch (err) {
			if ((err as NodeJS.ErrnoException).code === 'ENOENT') return undefined
			throw err
		}
		return parseDeploymentManifest(json)
	}

	async save(manifest: DeploymentManifest): Promise<void> {
		await mkdir(dirname(this.#path), { recursive: true })
		await writeFile(this.#path, serializeDeploymentManifest(manifest))
	}
}

function trimTrailingSlash(s: string): string {
	return s.endsWith('/') ? s.slice(0, -1) : s
}

function trimLeadingSlash(s: string): string {
	return s.startsWith('/') ? s.slice(1) : s
}

/** Convert an absolute filesystem path to a `file://` url (forward slashes). */
function pathToFileUrl(p: string): string {
	const normalized = p.replace(/\\/g, '/')
	return normalized.startsWith('/') ? `file://${normalized}` : `file:///${normalized}`
}

/** Map a deploy path's extension to a content type for the upload's `Content-Type` header. */
const CONTENT_TYPES: Readonly<Record<string, string>> = {
	js: 'application/javascript',
	mjs: 'application/javascript',
	cjs: 'application/javascript',
	json: 'application/json',
	html: 'text/html; charset=utf-8',
	css: 'text/css; charset=utf-8',
	map: 'application/json',
	wasm: 'application/wasm',
	svg: 'image/svg+xml',
	txt: 'text/plain; charset=utf-8',
}

function contentTypeFor(path: string, fallback: string): string {
	const dot = path.lastIndexOf('.')
	if (dot < 0) return fallback
	const ext = path.slice(dot + 1).toLowerCase()
	return CONTENT_TYPES[ext] ?? fallback
}
