/**
 * @module
 *
 * Build-to-deploy assembly: turn a finished build directory plus a versioned
 * {@link FederationManifest} into a {@link DeployArtifact} — the concrete set of
 * files to upload, the manifest to publish, and the per-module *versioned* upload
 * paths a target needs to land each module immutably (so a later rollback can
 * repoint at an already-published prior version).
 *
 * Treaty compiles every lazy route and library into its own federated module
 * (see `@treaty/module-federation`) and records which version/url of each is live
 * in a manifest (see `@treaty/federation-deploy`). The compiler emits those module
 * builds into one output directory, each under its own subdirectory keyed by the
 * same `moduleId` the manifest uses. {@link assembleDeployArtifact} reconciles the
 * two: it reads the files on disk for every module the manifest names, and lays
 * out, per module, the versioned remote path (`<moduleId>/<version>/<file>`) those
 * files should be uploaded to.
 *
 * The assembly is **pure planning of bytes + paths**: it reads the build dir but
 * performs no upload. {@link deploy} (see `./deploy.ts`) is what hands the result
 * to a {@link DeployTarget}. Assembly also supports **partial** deploys — pass
 * `only`/`changedFiles`+`graph` and the artifact contains only the affected
 * modules, the unit of independent deploy/rollback.
 */

import { readdir, readFile, stat } from 'node:fs/promises'
import { isAbsolute, join, posix, sep } from 'node:path'
import {
	computeAffectedModules,
	type FederationManifest,
	type ModuleDependencyGraph,
	type ModuleDeployment,
	type ModuleKind,
} from '@treaty/federation-deploy'

/**
 * The built files of one federated module, laid out for upload. Every file the
 * module contributes is keyed by its **deploy path** — the versioned, immutable
 * location a target uploads it to, `"<moduleId>/<version>/<relativePath>"`. Two
 * deploys of different versions of the same module never collide, which is what
 * makes per-module rollback possible.
 */
export interface ModuleArtifact {
	/** Stable identity of the module across versions (the manifest key). */
	readonly moduleId: string
	/** The version of the module these files represent (from the manifest). */
	readonly version: string
	/** What kind of module this is (host / route remote / lib). */
	readonly kind: ModuleKind
	/**
	 * The versioned base path the module's files upload under, with no trailing
	 * slash: `"<moduleId>/<version>"`. Every key in {@link ModuleArtifact.files}
	 * begins with this.
	 */
	readonly basePath: string
	/** The module's remote entry filename (relative to its build dir), if present. */
	readonly entry: string | undefined
	/**
	 * Every file to upload for this module, keyed by deploy path
	 * (`"<moduleId>/<version>/<relativePath>"`) with the file's bytes as the value.
	 */
	readonly files: Readonly<Record<string, Uint8Array>>
	/** Deployment the manifest records for this module (its live version + url). */
	readonly deployment: ModuleDeployment
}

/**
 * The output of {@link assembleDeployArtifact}: everything a {@link DeployTarget}
 * needs to publish a release. Carries the manifest to publish, the per-module
 * {@link ModuleArtifact}s (each with its versioned upload paths), and the flat
 * union of every file's deploy path — the complete upload set.
 */
export interface DeployArtifact {
	/** The manifest this artifact publishes (the modules it covers point at it). */
	readonly manifest: FederationManifest
	/** Per-module built files + versioned paths, keyed by `moduleId`. */
	readonly modules: Readonly<Record<string, ModuleArtifact>>
	/**
	 * `true` when this is a partial artifact (a subset of the manifest's modules).
	 * A full artifact covers every module the manifest names.
	 */
	readonly partial: boolean
	/** Logical target name this artifact was assembled for (informational). */
	readonly target: string | undefined
}

/** Options for {@link assembleDeployArtifact}. */
export interface AssembleDeployArtifactOptions {
	/**
	 * Absolute path to the finished build output. Each module's files are read
	 * from `<buildDir>/<moduleId>/` (see {@link AssembleDeployArtifactOptions.moduleDir}
	 * to override). Required.
	 */
	readonly buildDir: string
	/**
	 * The versioned manifest describing which version/url of every module is live.
	 * The artifact covers the modules this manifest names (or the subset selected
	 * for a partial deploy). Required.
	 */
	readonly manifest: FederationManifest
	/**
	 * Logical name of the deployment target (e.g. `'fs'`, `'cloud'`,
	 * `'treaty-cloud'`). Recorded on the artifact; informational only — the actual
	 * upload is performed by {@link deploy} against a {@link DeployTarget}.
	 */
	readonly target?: string
	/**
	 * Restrict the artifact to these module ids (a **partial** deploy). When set,
	 * only these modules are read and included. Mutually informative with
	 * {@link AssembleDeployArtifactOptions.changedFiles}; if both are given, `only`
	 * wins.
	 */
	readonly only?: Iterable<string>
	/**
	 * Compute the partial set from a change set instead of listing it: the affected
	 * modules (direct edits + shared-lib fan-out) become the artifact's modules.
	 * Requires {@link AssembleDeployArtifactOptions.graph}.
	 */
	readonly changedFiles?: Iterable<string>
	/** The module dependency graph used to expand {@link AssembleDeployArtifactOptions.changedFiles}. */
	readonly graph?: ModuleDependencyGraph
	/**
	 * Resolve a module's build directory (relative to `buildDir`, or absolute).
	 * Defaults to the `moduleId` itself, so files live at `<buildDir>/<moduleId>/`.
	 */
	readonly moduleDir?: (moduleId: string, manifest: FederationManifest) => string
	/**
	 * Pick the remote entry filename within a module's build dir. Defaults to the
	 * basename of the module's manifest url, falling back to `remoteEntry.js` when
	 * that file is present. Returning `undefined` records no entry.
	 */
	readonly entryFor?: (moduleId: string, files: readonly string[], deployment: ModuleDeployment) => string | undefined
}

const DEFAULT_ENTRY = 'remoteEntry.js'

/**
 * Assemble a {@link DeployArtifact} from a built output directory and a manifest.
 *
 * For every module covered (all manifest modules, or the partial subset), reads
 * its files from `<buildDir>/<moduleId>/` and lays them out under the versioned
 * deploy path `<moduleId>/<version>/<relativePath>` — the immutable location a
 * target uploads to, and the reason a later rollback can repoint at a prior
 * version without rebuilding. Performs no upload; that is {@link deploy}'s job.
 *
 * Partial deploy: pass `only` (an explicit id list) or `changedFiles` + `graph`
 * (the affected set via {@link computeAffectedModules}) to cover just those
 * modules. The returned artifact's `partial` flag reflects whether it is a strict
 * subset of the manifest.
 *
 * @throws if `buildDir`/`manifest` are missing, if a selected module is absent
 *   from the manifest, if `changedFiles` is given without `graph`, or if a
 *   covered module has no readable build directory.
 */
export async function assembleDeployArtifact(
	options: AssembleDeployArtifactOptions
): Promise<DeployArtifact> {
	const { buildDir, manifest } = options
	if (typeof buildDir !== 'string' || buildDir.length === 0) {
		throw new TypeError('assembleDeployArtifact: options.buildDir is required')
	}
	if (!manifest || typeof manifest !== 'object' || typeof manifest.modules !== 'object') {
		throw new TypeError('assembleDeployArtifact: options.manifest is required')
	}

	const allIds = Object.keys(manifest.modules)
	const selected = selectModuleIds(options, allIds)
	const moduleDir = options.moduleDir ?? ((id) => id)

	const modules: Record<string, ModuleArtifact> = {}
	for (const moduleId of selected) {
		const deployment = manifest.modules[moduleId]
		if (!deployment) {
			throw new RangeError(
				`assembleDeployArtifact: module "${moduleId}" is not in the manifest (cannot assemble its artifact)`
			)
		}

		const dir = resolveModuleDir(buildDir, moduleDir(moduleId, manifest))
		const relFiles = await readModuleFiles(dir, moduleId)
		const entry = pickEntry(options.entryFor, moduleId, relFiles, deployment)

		const basePath = posix.join(moduleId, deployment.version)
		const files: Record<string, Uint8Array> = {}
		for (const rel of relFiles) {
			const buf = await readFile(join(dir, rel.split(posix.sep).join(sep)))
			files[posix.join(basePath, rel)] = new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength)
		}

		modules[moduleId] = {
			moduleId,
			version: deployment.version,
			kind: manifest.kinds?.[moduleId] ?? 'route',
			basePath,
			entry,
			files,
			deployment,
		}
	}

	return {
		manifest,
		modules,
		partial: selected.length < allIds.length,
		target: options.target,
	}
}

/**
 * The flat union of every file in an artifact, keyed by deploy path — the exact
 * set a {@link DeployTarget} uploads. Convenience over walking
 * {@link DeployArtifact.modules}.
 */
export function artifactFiles(artifact: DeployArtifact): Readonly<Record<string, Uint8Array>> {
	const out: Record<string, Uint8Array> = {}
	for (const moduleId of Object.keys(artifact.modules).sort()) {
		for (const [path, bytes] of Object.entries(artifact.modules[moduleId]!.files)) {
			out[path] = bytes
		}
	}
	return out
}

/** The deploy paths (sorted) every file in an artifact will be uploaded to. */
export function artifactPaths(artifact: DeployArtifact): string[] {
	return Object.keys(artifactFiles(artifact)).sort()
}

/** Decide which module ids the artifact covers, honoring `only` / `changedFiles`. */
function selectModuleIds(options: AssembleDeployArtifactOptions, allIds: string[]): string[] {
	if (options.only !== undefined) {
		return dedupeSorted(options.only)
	}
	if (options.changedFiles !== undefined) {
		if (!options.graph) {
			throw new TypeError(
				'assembleDeployArtifact: options.changedFiles requires options.graph to compute the affected modules'
			)
		}
		// Affected modules are the deploy unit; intersect with the manifest so we
		// only try to assemble modules the manifest actually knows about.
		const affected = computeAffectedModules(options.changedFiles, options.graph)
		const known = new Set(allIds)
		return affected.filter((id) => known.has(id))
	}
	return [...allIds].sort()
}

/** Resolve a module's build dir: absolute as-is, else relative to `buildDir`. */
function resolveModuleDir(buildDir: string, dir: string): string {
	return isAbsolute(dir) ? dir : join(buildDir, dir)
}

/** Read every file under a module's build dir, returned as posix-relative paths, sorted. */
async function readModuleFiles(dir: string, moduleId: string): Promise<string[]> {
	let entries: Awaited<ReturnType<typeof readdir>>
	try {
		entries = await readdir(dir, { withFileTypes: true })
	} catch (err) {
		if ((err as NodeJS.ErrnoException).code === 'ENOENT') {
			throw new RangeError(
				`assembleDeployArtifact: no build output for module "${moduleId}" at ${dir}`
			)
		}
		throw err
	}

	const out: string[] = []
	for (const entry of entries) {
		const full = join(dir, entry.name)
		if (entry.isDirectory()) {
			const nested = await readModuleFiles(full, moduleId)
			for (const rel of nested) out.push(posix.join(entry.name, rel))
		} else if (entry.isFile()) {
			out.push(entry.name)
		} else {
			// Resolve symlinks / unknown dirents via stat so they are not silently dropped.
			const info = await stat(full)
			if (info.isDirectory()) {
				const nested = await readModuleFiles(full, moduleId)
				for (const rel of nested) out.push(posix.join(entry.name, rel))
			} else if (info.isFile()) {
				out.push(entry.name)
			}
		}
	}
	out.sort()
	return out.map((rel) => toPosix(rel))
}

/** Choose the remote entry filename: caller override, else url basename, else default. */
function pickEntry(
	entryFor: AssembleDeployArtifactOptions['entryFor'],
	moduleId: string,
	files: readonly string[],
	deployment: ModuleDeployment
): string | undefined {
	if (entryFor) {
		return entryFor(moduleId, files, deployment)
	}
	const fromUrl = urlBasename(deployment.url)
	if (fromUrl && files.includes(fromUrl)) return fromUrl
	if (files.includes(DEFAULT_ENTRY)) return DEFAULT_ENTRY
	return fromUrl ?? undefined
}

/** Basename of a manifest url (the remote entry filename), or `undefined`. */
function urlBasename(url: string): string | undefined {
	const stripped = url.split(/[?#]/)[0] ?? url
	const last = stripped.split('/').pop()
	return last && last.length > 0 ? last : undefined
}

function dedupeSorted(ids: Iterable<string>): string[] {
	return [...new Set(ids)].sort()
}

function toPosix(p: string): string {
	return p.split(sep).join(posix.sep)
}
