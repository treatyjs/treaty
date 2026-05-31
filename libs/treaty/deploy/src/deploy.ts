/**
 * @module
 *
 * The execution layer: hand an assembled {@link DeployArtifact} to a pluggable
 * {@link DeployTarget} and publish it. Treaty is a **compiler, not a host** — it
 * emits the artifact (see {@link assembleDeployArtifact}) and invokes a target; it
 * never runs a server. *Where* the bytes land — a local directory, an object
 * store, a static-host CDN, the Treaty cloud — is the target's concern, so the
 * set of targets is open.
 *
 * A {@link DeployTarget} is the smallest possible upload sink: `upload(path,
 * bytes)` puts one file at a deploy path, and `urlFor(path)` reports where that
 * path is served from. {@link deploy} drives it — it uploads every file in the
 * artifact, then resolves each module's served url and returns the manifest to
 * publish. The same shape covers self-hosted ({@link FsDeployTarget}) and cloud /
 * static-host backends (see {@link CloudDeployTarget}). A {@link DeployPlugin}
 * from `@treaty/federation-deploy` is driven instead by {@link deployViaPlugin}.
 *
 * **Partial deploy** falls out for free: a partial {@link DeployArtifact} carries
 * only the affected modules, so {@link deploy} uploads only those. **Rollback** is
 * {@link rollback} — it flips a single manifest entry to an already-published
 * prior version, uploading nothing.
 */

import {
	buildManifest,
	getModule,
	rollbackModule,
	setModuleVersion,
	type DeployModule,
	type DeployPlugin,
	type FederationManifest,
	type ModuleDeployment,
} from '@treaty/federation-deploy'
import { posix } from 'node:path'
import type { DeployArtifact, ModuleArtifact } from './artifact.js'

/**
 * A pluggable place an artifact is published to. The minimal contract is
 * {@link DeployTarget.upload} (put one file's bytes at a deploy path) plus
 * {@link DeployTarget.urlFor} (the url that path is served from). Optional hooks
 * let a target prepare/finish a deploy and remove a path on rollback cleanup.
 *
 * Targets are open-ended on purpose: {@link FsDeployTarget} writes to the local
 * filesystem (self-hosted), a {@link CloudDeployTarget} is the same shape over an
 * object store / static host / the Treaty cloud, and a `@treaty/federation-deploy`
 * {@link DeployPlugin} is driven via {@link deployViaPlugin}.
 */
export interface DeployTarget {
	/** The target's name (informational; e.g. `'fs'`, `'s3'`, `'treaty-cloud'`). */
	readonly name: string
	/** Optional hook run once before any upload (open a connection, ensure a bucket). */
	begin?(ctx: DeployTargetContext): Promise<void> | void
	/** Upload one file's bytes to its deploy path. Must be idempotent for the same path. */
	upload(path: string, bytes: Uint8Array, ctx: DeployTargetContext): Promise<void> | void
	/** The url the given deploy path is served from once uploaded. */
	urlFor(path: string, ctx: DeployTargetContext): string
	/** Optional hook run once after all uploads succeed (publish a manifest, flush a CDN). */
	finish?(ctx: DeployTargetContext): Promise<void> | void
	/** Optional removal of a deploy path (used by rollback cleanup; not required for correctness). */
	remove?(path: string, ctx: DeployTargetContext): Promise<void> | void
}

/**
 * A {@link DeployTarget} aimed at a remote host (object storage, a static-host
 * CDN, the Treaty cloud). It is structurally identical to {@link DeployTarget} —
 * this alias exists to name the "self-hosted and Treaty cloud" backends the
 * README calls out as pluggable; both implement the same upload/urlFor contract,
 * so a cloud SDK plugs in by satisfying this interface.
 */
export type CloudDeployTarget = DeployTarget

/** Ambient context threaded through a {@link DeployTarget}'s lifecycle. */
export interface DeployTargetContext {
	/** Informational app name (mirrors {@link FederationManifest.app}). */
	readonly app: string | undefined
	/** Target environment label, e.g. `'staging'` / `'prod'`. */
	readonly env: string | undefined
	/** Sink for human-readable progress; a no-op when omitted. */
	readonly logger: (message: string) => void
	/** Whether this is a dry run (no real I/O should be performed). */
	readonly dryRun: boolean
	/** Free-form target-specific parameters. */
	readonly params: Readonly<Record<string, unknown>>
}

/** Options for {@link deploy}. */
export interface DeployOptions {
	/** App name recorded on the context (defaults to the manifest's `app`). */
	readonly app?: string
	/** Environment label for the deploy. */
	readonly env?: string
	/** Progress sink. */
	readonly logger?: (message: string) => void
	/** Plan only: skip {@link DeployTarget.upload}/`begin`/`finish` but still compute urls + manifest. */
	readonly dryRun?: boolean
	/** Target-specific parameters passed through on the context. */
	readonly params?: Readonly<Record<string, unknown>>
}

/** One module's result within a {@link DeployResult}. */
export interface ModuleDeployResult {
	/** The module that was deployed. */
	readonly moduleId: string
	/** The deployment (version + served url) now live for it. */
	readonly deployment: ModuleDeployment
	/** The deploy paths uploaded for this module, sorted. */
	readonly uploaded: readonly string[]
}

/** The result of {@link deploy} / {@link deployViaPlugin}. */
export interface DeployResult {
	/** The manifest to publish: every deployed module repointed at its served url. */
	readonly manifest: FederationManifest
	/** Per-module results, keyed by `moduleId`. */
	readonly modules: Readonly<Record<string, ModuleDeployResult>>
	/** Every deploy path uploaded across all modules, sorted. */
	readonly uploaded: readonly string[]
	/** `true` when the artifact deployed was partial (a subset of the manifest). */
	readonly partial: boolean
}

/**
 * Publish an assembled {@link DeployArtifact} to a {@link DeployTarget}.
 *
 * Runs `target.begin?`, uploads every file in the artifact (only the affected
 * modules, when the artifact is partial), resolves each module's served url via
 * `target.urlFor` over its remote entry, then runs `target.finish?`. Returns the
 * manifest to publish — the input artifact's manifest with every deployed module
 * repointed at the url the target reports — plus per-module upload results.
 *
 * The input artifact and its manifest are never mutated. With `options.dryRun`,
 * no `begin`/`upload`/`finish` runs but urls + the resulting manifest are still
 * computed (a deploy preview).
 *
 * @throws if the artifact has no modules, or if a module has no files from which
 *   to resolve a served url.
 */
export async function deploy(
	artifact: DeployArtifact,
	target: DeployTarget,
	options: DeployOptions = {}
): Promise<DeployResult> {
	if (!artifact || typeof artifact.modules !== 'object') {
		throw new TypeError('deploy: a DeployArtifact is required')
	}
	if (!target || typeof target.upload !== 'function' || typeof target.urlFor !== 'function') {
		throw new TypeError('deploy: a DeployTarget with upload() and urlFor() is required')
	}

	const moduleIds = Object.keys(artifact.modules).sort()
	if (moduleIds.length === 0) {
		throw new RangeError('deploy: artifact has no modules to deploy')
	}

	const ctx: DeployTargetContext = {
		app: options.app ?? artifact.manifest.app,
		env: options.env,
		logger: options.logger ?? (() => {}),
		dryRun: options.dryRun ?? false,
		params: options.params ?? {},
	}

	if (!ctx.dryRun) await target.begin?.(ctx)

	let manifest = artifact.manifest
	const modules: Record<string, ModuleDeployResult> = {}
	const uploadedAll: string[] = []

	for (const moduleId of moduleIds) {
		const module = artifact.modules[moduleId]!
		const uploaded = Object.keys(module.files).sort()
		if (!ctx.dryRun) {
			for (const path of uploaded) {
				await target.upload(path, module.files[path]!, ctx)
			}
		}
		uploadedAll.push(...uploaded)

		const url = target.urlFor(resolveEntryPath(module), ctx)
		if (typeof url !== 'string' || url.length === 0) {
			throw new TypeError(`deploy: target "${target.name}" returned an empty url for "${moduleId}"`)
		}

		const deployment: ModuleDeployment = { version: module.version, url }
		manifest = repointForDeploy(manifest, module, deployment)
		modules[moduleId] = { moduleId, deployment, uploaded }
		ctx.logger(`[${target.name}] deployed ${moduleId}@${module.version} -> ${url}`)
	}

	if (!ctx.dryRun) await target.finish?.(ctx)

	return { manifest, modules, uploaded: uploadedAll.sort(), partial: artifact.partial }
}

/**
 * Deploy an artifact by delegating each module to a `@treaty/federation-deploy`
 * {@link DeployPlugin} — the existing pluggable deploy-method registry. This is
 * the per-module counterpart to {@link deploy}: where {@link deploy} drives a
 * per-file {@link DeployTarget}, this hands each module's files to `plugin.deploy`
 * as one artifact and collects the deployment the plugin reports, repointing the
 * manifest at it.
 *
 * Use this when you already have a registered {@link DeployPlugin} (the reference
 * `FsDeployPlugin`/`NoopDeployPlugin`, or a registered cloud plugin); use
 * {@link deploy} + a {@link DeployTarget} for a from-scratch upload sink. Partial
 * artifacts deploy only their modules; the input manifest is never mutated.
 *
 * @throws if `plugin` has no `deploy`, or the artifact has no modules.
 */
export async function deployViaPlugin(
	artifact: DeployArtifact,
	plugin: DeployPlugin,
	options: DeployViaPluginOptions = {}
): Promise<DeployResult> {
	if (!artifact || typeof artifact.modules !== 'object') {
		throw new TypeError('deployViaPlugin: a DeployArtifact is required')
	}
	if (!plugin || typeof plugin.deploy !== 'function') {
		throw new TypeError('deployViaPlugin: a DeployPlugin with deploy() is required')
	}
	const moduleIds = Object.keys(artifact.modules).sort()
	if (moduleIds.length === 0) {
		throw new RangeError('deployViaPlugin: artifact has no modules to deploy')
	}

	const ctx = {
		...(options.app ?? artifact.manifest.app
			? { app: options.app ?? artifact.manifest.app }
			: {}),
		...(options.env !== undefined ? { env: options.env } : {}),
		...(options.logger ? { logger: options.logger } : {}),
		...(options.params ? { params: options.params } : {}),
	}

	let manifest = artifact.manifest
	const modules: Record<string, ModuleDeployResult> = {}
	const uploadedAll: string[] = []

	for (const moduleId of moduleIds) {
		const module = artifact.modules[moduleId]!
		const deployModule: DeployModule = {
			moduleId: module.moduleId,
			version: module.version,
			kind: module.kind,
		}
		const deployment = await plugin.deploy(
			deployModule,
			{
				...(module.entry !== undefined ? { entry: module.entry } : {}),
				files: encodeFilesForPlugin(module),
			},
			ctx
		)
		manifest = repointForDeploy(manifest, module, deployment)
		const uploaded = Object.keys(module.files).sort()
		uploadedAll.push(...uploaded)
		modules[moduleId] = { moduleId, deployment, uploaded }
	}

	return { manifest, modules, uploaded: uploadedAll.sort(), partial: artifact.partial }
}

/** Options for {@link deployViaPlugin}. */
export interface DeployViaPluginOptions {
	/** App name passed on the plugin context (defaults to the manifest's `app`). */
	readonly app?: string
	/** Environment label passed on the plugin context. */
	readonly env?: string
	/** Progress sink passed on the plugin context. */
	readonly logger?: (message: string) => void
	/** Plugin-specific params passed on the context. */
	readonly params?: Readonly<Record<string, unknown>>
}

/**
 * **Rollback**: return a NEW manifest with one module reverted to an
 * already-published `toVersion`, leaving every other module untouched. No upload
 * happens — rollback never rebuilds; it flips the manifest entry to a prior,
 * still-published artifact (the versioned deploy paths from
 * {@link assembleDeployArtifact} are what keep that prior version addressable).
 *
 * The url for `toVersion` is taken from `options.url`, else derived from the
 * module's current url via `options.urlFor`, else — by default — by swapping the
 * current version segment for `toVersion` in the live url (the layout
 * {@link assembleDeployArtifact} publishes under). The input manifest is never
 * mutated.
 *
 * @throws if the module is unknown to the manifest, or no `toVersion` url resolves.
 */
export function rollback(
	manifest: FederationManifest,
	moduleId: string,
	toVersion: string,
	options: RollbackOptions = {}
): FederationManifest {
	const current = getModule(manifest, moduleId)
	if (!current) {
		throw new RangeError(`rollback: unknown moduleId "${moduleId}"`)
	}
	if (typeof toVersion !== 'string' || toVersion.length === 0) {
		throw new TypeError(`rollback: "${moduleId}" needs a non-empty target version`)
	}

	const url =
		options.url ??
		(options.urlFor
			? options.urlFor(toVersion, current)
			: swapVersionSegment(current.url, current.version, toVersion))
	if (typeof url !== 'string' || url.length === 0) {
		throw new TypeError(
			`rollback: "${moduleId}" needs a url for version "${toVersion}" (pass options.url or options.urlFor)`
		)
	}

	return rollbackModule(manifest, moduleId, toVersion, { url })
}

/** Options for {@link rollback}. */
export interface RollbackOptions {
	/** Explicit url the target serves `toVersion` from (skips derivation). */
	readonly url?: string
	/** Derive the `toVersion` url from the module's current deployment. */
	readonly urlFor?: (toVersion: string, current: ModuleDeployment) => string
}

/**
 * The deploy path of the module's remote entry, used to resolve its served url.
 * Falls back to the module's first file when no entry was identified.
 */
function resolveEntryPath(module: ModuleArtifact): string {
	if (module.entry) return posix.join(module.basePath, module.entry)
	const first = Object.keys(module.files).sort()[0]
	if (!first) {
		throw new RangeError(
			`deploy: module "${module.moduleId}" has no files, so no served url can be resolved`
		)
	}
	return first
}

/** Repoint one module in the manifest to a deployment, adding it if the manifest lacks it. */
function repointForDeploy(
	manifest: FederationManifest,
	module: ModuleArtifact,
	deployment: ModuleDeployment
): FederationManifest {
	if (getModule(manifest, module.moduleId)) {
		return setModuleVersion(manifest, module.moduleId, deployment.version, { url: deployment.url })
	}
	// A module not yet in the manifest (a first deploy): append it, preserving kinds.
	const inputs = [
		...Object.entries(manifest.modules).map(([id, dep]) => ({
			moduleId: id,
			version: dep.version,
			url: dep.url,
			kind: manifest.kinds?.[id] ?? ('route' as const),
		})),
		{ moduleId: module.moduleId, version: deployment.version, url: deployment.url, kind: module.kind },
	]
	return buildManifest(inputs, manifest.app !== undefined ? { app: manifest.app } : {})
}

/** Encode a module's file bytes to the UTF-8 text contract a {@link DeployPlugin} expects. */
function encodeFilesForPlugin(module: ModuleArtifact): Record<string, string> {
	const out: Record<string, string> = {}
	const prefix = `${module.basePath}/`
	for (const [path, bytes] of Object.entries(module.files)) {
		// Strip the versioned base path so the plugin (which versions paths itself)
		// receives module-relative file names.
		const rel = path.startsWith(prefix) ? path.slice(prefix.length) : path
		out[rel] = decodeUtf8(bytes)
	}
	return out
}

function decodeUtf8(bytes: Uint8Array): string {
	return new TextDecoder().decode(bytes)
}

/** Swap a version segment in a stamped url, used as the default rollback url derivation. */
function swapVersionSegment(url: string, fromVersion: string, toVersion: string): string {
	const segment = `/${fromVersion}/`
	if (url.includes(segment)) {
		return url.replace(segment, `/${toVersion}/`)
	}
	// No recognizable version segment: best effort, replace a trailing occurrence.
	return url.replace(fromVersion, toVersion)
}
