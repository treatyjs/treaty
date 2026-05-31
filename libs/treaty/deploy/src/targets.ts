/**
 * @module
 *
 * Reference {@link DeployTarget} implementations. {@link FsDeployTarget} is the
 * smallest *real* target — it writes a {@link DeployArtifact}'s files to a local
 * directory and serves them under a configurable base url. It is the self-hosted
 * backend and the example a cloud/static-host target (S3, GCS, a CDN API, the
 * Treaty cloud) is modeled on: implement the same `upload`/`urlFor` contract over
 * your storage SDK and {@link deploy} drives it unchanged.
 *
 * A target deliberately knows nothing about modules, versions, or manifests — it
 * only puts bytes at a path and reports the url for a path. All the federation
 * semantics live in {@link assembleDeployArtifact} (which produces the versioned
 * paths) and {@link deploy} (which resolves urls + repoints the manifest).
 */

import { mkdir, rm, writeFile } from 'node:fs/promises'
import { dirname, join, posix, sep } from 'node:path'
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
