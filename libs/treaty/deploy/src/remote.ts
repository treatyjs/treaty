/**
 * @module
 *
 * **Per-remote deploy + rollback** — federation as the unit of deployment
 * granularity, made operational. Treaty compiles every lazy route and library
 * into its own federated remote; this module deploys and rolls back **one remote
 * at a time** against a persisted {@link DeploymentManifest}, so a single
 * route/lib is rolled forward or back **without redeploying the whole app**.
 *
 * Three operations, all driven through injected boundaries so the orchestration is
 * pure and fully fake-testable:
 *   - {@link deployRemote} — upload one remote's `artifact` to an injected
 *     {@link DeployTarget}, then record the new version+entry in the manifest
 *     (appending to the remote's history) and persist it via an injected
 *     {@link DeploymentManifestStore}.
 *   - {@link rollbackRemote} — flip one remote's served version back to a prior
 *     version already in its history and persist that — **no upload, no rebuild**
 *     (the prior artifact is still published under its immutable version-stamped
 *     url). Every other remote is left untouched.
 *   - {@link deployAffectedRemotes} — given the federation affected-graph (the
 *     changed remotes), deploy ONLY those remotes (a **partial** deploy), leaving
 *     the rest of the live app in place.
 *
 * All IO — uploading bytes ({@link DeployTarget}) and reading/writing the ledger
 * ({@link DeploymentManifestStore}) — sits behind injected interfaces, so the same
 * code runs against a CDN + a JSON file in production and against fakes in tests.
 */

import {
	createDeploymentManifest,
	getRemote,
	recordDeployment,
	rollbackTo,
	type DeploymentManifest,
	type DeploymentManifestStore,
	type RemoteDeployment,
	type RemoteKind,
	type RollbackToOptions,
} from '@treaty/federation-deploy'
import type { DeployTarget, DeployTargetContext } from './deploy.js'

/**
 * The built output of one federated remote to publish: which `remote` (its stable
 * name), the `version` being deployed, the remote-entry filename, the `kind`, and
 * the `files` to upload keyed by path **relative to the remote's build root**.
 * This module version-stamps the upload paths itself
 * (`<remote>/<version>/<relativePath>`), so the same version of a remote never
 * overwrites a prior one — which is exactly what keeps a prior version
 * addressable for a later rollback.
 */
export interface RemoteArtifact {
	/** Stable name of the remote (the deployment-manifest key). */
	readonly remote: string
	/** The version being deployed. */
	readonly version: string
	/** What kind of remote this is. Defaults to `'route'` for a brand-new remote. */
	readonly kind?: RemoteKind
	/** The remote-entry filename within the artifact (e.g. `remoteEntry.js`). */
	readonly entry: string
	/** Files to upload, keyed by remote-relative path, value is the bytes. */
	readonly files: Readonly<Record<string, Uint8Array>>
}

/** Shared injected dependencies for a per-remote deploy/rollback run. */
export interface RemoteDeployDeps {
	/** Where the deployment manifest (the per-remote ledger) is read from / written to. */
	readonly store: DeploymentManifestStore
	/** App name to stamp on a freshly created manifest (first deploy). */
	readonly app?: string
	/** Environment label threaded onto the {@link DeployTargetContext}. */
	readonly env?: string
	/** Progress sink; a no-op when omitted. */
	readonly logger?: (message: string) => void
	/** Target-specific params threaded onto the {@link DeployTargetContext}. */
	readonly params?: Readonly<Record<string, unknown>>
}

/** Options for {@link deployRemote}. */
export interface DeployRemoteOptions extends RemoteDeployDeps {
	/**
	 * Plan only: skip {@link DeployTarget.upload}/`begin`/`finish` but still resolve
	 * the served url, record the deployment in the manifest, and persist it. Useful
	 * for previewing a deploy's effect on the ledger.
	 */
	readonly dryRun?: boolean
}

/** The result of a single {@link deployRemote} / {@link rollbackRemote}. */
export interface RemoteDeployResult {
	/** The remote that was deployed / rolled back. */
	readonly remote: string
	/** Its ledger entry after the operation (current version, entry url, full history). */
	readonly deployment: RemoteDeployment
	/** The deploy paths uploaded for this remote, sorted (empty for a rollback). */
	readonly uploaded: readonly string[]
	/** The deployment manifest after the operation (also persisted to the store). */
	readonly manifest: DeploymentManifest
}

/**
 * **Deploy one remote.** Uploads `artifact`'s files to `target` under versioned
 * paths (`<remote>/<version>/<file>`), resolves the served entry url from the
 * target, records the new version+entry in the deployment manifest (appending to
 * the remote's history), persists the manifest through the injected store, and
 * returns the resulting ledger entry. Every other remote in the manifest is left
 * exactly as it was — this is a partial, single-remote deploy.
 *
 * The manifest is loaded fresh from the store on entry (so concurrent deploys of
 * different remotes compose), created empty if the store has none yet. With
 * `dryRun`, nothing is uploaded but the url is still resolved and the manifest is
 * still recorded + persisted (a ledger preview).
 *
 * @throws if `target`/`artifact` are missing required pieces, or the target
 *   returns an empty url.
 */
export async function deployRemote(
	target: DeployTarget,
	artifact: RemoteArtifact,
	options: DeployRemoteOptions
): Promise<RemoteDeployResult> {
	if (!target || typeof target.upload !== 'function' || typeof target.urlFor !== 'function') {
		throw new TypeError('deployRemote: a DeployTarget with upload() and urlFor() is required')
	}
	validateArtifact(artifact, 'deployRemote')
	const store = requireStore(options.store, 'deployRemote')

	const ctx = makeContext(options, options.dryRun ?? false)
	const basePath = `${artifact.remote}/${artifact.version}`

	if (!ctx.dryRun) await target.begin?.(ctx)

	const uploaded: string[] = []
	if (!ctx.dryRun) {
		for (const rel of Object.keys(artifact.files).sort()) {
			const path = `${basePath}/${rel}`
			await target.upload(path, artifact.files[rel]!, ctx)
			uploaded.push(path)
		}
	} else {
		for (const rel of Object.keys(artifact.files).sort()) uploaded.push(`${basePath}/${rel}`)
	}

	const entryUrl = target.urlFor(`${basePath}/${artifact.entry}`, ctx)
	if (typeof entryUrl !== 'string' || entryUrl.length === 0) {
		throw new TypeError(`deployRemote: target "${target.name}" returned an empty url for "${artifact.remote}"`)
	}

	if (!ctx.dryRun) await target.finish?.(ctx)

	const current = await loadOrCreate(store, options.app)
	const next = recordDeployment(current, artifact.remote, artifact.version, entryUrl, {
		...(artifact.kind ? { kind: artifact.kind } : {}),
	})
	await store.save(next)

	ctx.logger(`[${target.name}] deployed remote ${artifact.remote}@${artifact.version} -> ${entryUrl}`)
	return {
		remote: artifact.remote,
		deployment: getRemote(next, artifact.remote)!,
		uploaded: uploaded.sort(),
		manifest: next,
	}
}

/** Options for {@link rollbackRemote}. */
export interface RollbackRemoteOptions extends RemoteDeployDeps {
	/** Explicit entry url the target serves `toVersion` from (skips derivation). */
	readonly entry?: string
	/** Derive the `toVersion` entry url from the remote's current deployment. */
	readonly entryFor?: (toVersion: string, current: RemoteDeployment) => string
	/**
	 * A {@link DeployTarget} to resolve the prior version's url through (its
	 * `urlFor`), instead of deriving it from the current entry by string-swap. The
	 * target is never uploaded to — rollback re-points, it does not rebuild.
	 */
	readonly target?: DeployTarget
}

/**
 * **Roll back one remote** to a prior `toVersion` that is already in its history —
 * flipping the served version+entry in the deployment manifest and persisting it,
 * **without uploading or rebuilding anything**. Every other remote is untouched.
 *
 * The prior version's entry url is recovered from `options.entry`, else
 * `options.entryFor`, else (if `options.target` is supplied) the target's
 * `urlFor("<remote>/<toVersion>/<entryFilename>")`, else by swapping the version
 * segment of the current entry url. The manifest is loaded fresh and persisted
 * through the injected store.
 *
 * @throws if the store has no manifest, the remote is unknown, or `toVersion` is
 *   not in the remote's deploy history.
 */
export async function rollbackRemote(
	remote: string,
	toVersion: string,
	options: RollbackRemoteOptions
): Promise<RemoteDeployResult> {
	const store = requireStore(options.store, 'rollbackRemote')
	const current = await Promise.resolve(store.load())
	if (!current) {
		throw new RangeError('rollbackRemote: no deployment manifest has been persisted yet')
	}
	const existing = getRemote(current, remote)
	if (!existing) {
		throw new RangeError(`rollbackRemote: unknown remote "${remote}"`)
	}

	const ctx = makeContext(options, false)
	const rollbackOptions = resolveRollbackEntryOptions(options, existing, toVersion, ctx)

	const next = rollbackTo(current, remote, toVersion, rollbackOptions)
	await store.save(next)

	const deployment = getRemote(next, remote)!
	ctx.logger(`[rollback] remote ${remote} -> ${toVersion} (${deployment.entry})`)
	return { remote, deployment, uploaded: [], manifest: next }
}

/** A single remote to deploy within {@link deployAffectedRemotes} (a {@link RemoteArtifact}). */
export type AffectedRemoteArtifact = RemoteArtifact

/** Options for {@link deployAffectedRemotes}. */
export interface DeployAffectedRemotesOptions extends RemoteDeployDeps {
	/** The deploy target every affected remote's artifact is uploaded to. */
	readonly target: DeployTarget
	/**
	 * The set of remotes the change affected (e.g. the output of
	 * `computeAffectedModules`). Only these remotes are deployed; any artifact whose
	 * `remote` is not in this set is skipped.
	 */
	readonly affected: Iterable<string>
	/** Plan only: forwarded to each {@link deployRemote}. */
	readonly dryRun?: boolean
}

/** The result of a {@link deployAffectedRemotes} run. */
export interface DeployAffectedRemotesResult {
	/** The remotes actually deployed (intersection of `affected` and supplied artifacts), sorted. */
	readonly deployed: readonly string[]
	/** Per-remote results, keyed by remote name. */
	readonly results: Readonly<Record<string, RemoteDeployResult>>
	/** The deployment manifest after all affected remotes were deployed. */
	readonly manifest: DeploymentManifest
}

/**
 * **Partial deploy of an affected set.** Given the federation affected-graph result
 * (`options.affected`) and the built `artifacts` keyed by remote name, deploy ONLY
 * the affected remotes — each via {@link deployRemote} — leaving every unaffected
 * remote in the manifest untouched. Remotes are deployed in sorted order so the
 * resulting manifest is deterministic, and the manifest threads through each
 * deploy (load-fresh-each-time via the store) so the final manifest carries every
 * affected remote's new version.
 *
 * An affected remote with no supplied artifact is skipped (CI may report a remote
 * affected that nonetheless produced no deployable output). An artifact for a
 * remote NOT in `affected` is ignored — `affected` is the authority on what ships.
 *
 * @throws (per {@link deployRemote}) on a bad target/artifact.
 */
export async function deployAffectedRemotes(
	artifacts: Iterable<AffectedRemoteArtifact>,
	options: DeployAffectedRemotesOptions
): Promise<DeployAffectedRemotesResult> {
	const store = requireStore(options.store, 'deployAffectedRemotes')
	if (!options.target) {
		throw new TypeError('deployAffectedRemotes: options.target (a DeployTarget) is required')
	}

	const affectedSet = new Set(options.affected)
	const byRemote = new Map<string, AffectedRemoteArtifact>()
	for (const artifact of artifacts) {
		validateArtifact(artifact, 'deployAffectedRemotes')
		byRemote.set(artifact.remote, artifact)
	}

	// Only deploy remotes that are BOTH affected and have an artifact; sorted for
	// determinism and a stable resulting manifest.
	const toDeploy = [...affectedSet].filter((name) => byRemote.has(name)).sort()

	const results: Record<string, RemoteDeployResult> = {}
	let manifest = (await loadOrCreate(store, options.app))
	for (const remote of toDeploy) {
		const result = await deployRemote(options.target, byRemote.get(remote)!, {
			store,
			...(options.app !== undefined ? { app: options.app } : {}),
			...(options.env !== undefined ? { env: options.env } : {}),
			...(options.logger ? { logger: options.logger } : {}),
			...(options.params ? { params: options.params } : {}),
			...(options.dryRun !== undefined ? { dryRun: options.dryRun } : {}),
		})
		results[remote] = result
		manifest = result.manifest
	}

	return { deployed: toDeploy, results, manifest }
}

/** Load the persisted manifest, or create a fresh (optionally app-named) one. */
async function loadOrCreate(
	store: DeploymentManifestStore,
	app: string | undefined
): Promise<DeploymentManifest> {
	const loaded = await Promise.resolve(store.load())
	return loaded ?? createDeploymentManifest(app !== undefined ? { app } : {})
}

/** Build the rollback options from the call options, resolving the entry url. */
function resolveRollbackEntryOptions(
	options: RollbackRemoteOptions,
	current: RemoteDeployment,
	toVersion: string,
	ctx: DeployTargetContext
): RollbackToOptions {
	if (options.entry !== undefined) return { entry: options.entry }
	if (options.entryFor) return { entryFor: options.entryFor }
	if (options.target) {
		const target = options.target
		return {
			entryFor: (v) => target.urlFor(`${current.name}/${v}/${basename(current.entry)}`, ctx),
		}
	}
	// No resolver supplied: let rollbackTo derive by swapping the version segment.
	return {}
}

/** Assemble the target context from the shared deps. */
function makeContext(deps: RemoteDeployDeps, dryRun: boolean): DeployTargetContext {
	return {
		app: deps.app,
		env: deps.env,
		logger: deps.logger ?? (() => {}),
		dryRun,
		params: deps.params ?? {},
	}
}

function requireStore(store: DeploymentManifestStore | undefined, fn: string): DeploymentManifestStore {
	if (!store || typeof store.load !== 'function' || typeof store.save !== 'function') {
		throw new TypeError(`${fn}: options.store (a DeploymentManifestStore with load()/save()) is required`)
	}
	return store
}

function validateArtifact(artifact: RemoteArtifact, fn: string): void {
	if (!artifact || typeof artifact !== 'object') {
		throw new TypeError(`${fn}: a RemoteArtifact is required`)
	}
	if (typeof artifact.remote !== 'string' || artifact.remote.length === 0) {
		throw new TypeError(`${fn}: artifact.remote (a non-empty name) is required`)
	}
	if (typeof artifact.version !== 'string' || artifact.version.length === 0) {
		throw new TypeError(`${fn}: remote "${artifact.remote}" artifact.version is required`)
	}
	if (typeof artifact.entry !== 'string' || artifact.entry.length === 0) {
		throw new TypeError(`${fn}: remote "${artifact.remote}" artifact.entry is required`)
	}
	if (typeof artifact.files !== 'object' || artifact.files === null) {
		throw new TypeError(`${fn}: remote "${artifact.remote}" artifact.files is required`)
	}
}

/** The last path segment of a url/path (the entry filename). */
function basename(urlOrPath: string): string {
	const stripped = urlOrPath.split(/[?#]/)[0] ?? urlOrPath
	const last = stripped.split('/').pop()
	return last && last.length > 0 ? last : urlOrPath
}
