/**
 * @module
 *
 * The runtime side of the **deployment manifest**. Where `./runtime.ts` resolves
 * remotes against the lean {@link FederationManifest}, this resolves them against
 * the operational {@link DeploymentManifest} — the per-remote ledger CI/CD
 * maintains (current version + entry url + history). At load time the host must
 * resolve each federated remote to the version the ledger *currently* serves, so a
 * deploy or rollback (which flips one remote's `currentVersion`/`entry`) takes
 * effect on the very next load with no rebuild.
 *
 * {@link createTreatyDeploymentRuntimePlugin} produces a
 * `@module-federation/enhanced` runtime plugin (a factory returning the plugin
 * object) that, in `beforeRequest` and `resolveRemote`, rewrites each remote's
 * `entry` url and stamps its `version` from the deployment manifest. The manifest
 * may be supplied directly, or lazily via an injected {@link DeploymentManifestStore}
 * (loaded once and cached). The `@module-federation/enhanced/runtime` peer is
 * referenced structurally so this module typechecks without it installed.
 */

import type {
	FederationBeforeRequestArgs,
	FederationResolveRemoteArgs,
	FederationRuntimePlugin,
	FederationRuntimeRemote,
} from '@module-federation/enhanced/runtime'
import type {
	DeploymentManifest,
	DeploymentManifestStore,
	RemoteDeployment,
} from './deployment-manifest.js'

/**
 * The two ways to give the plugin its deployment manifest: the manifest object
 * itself (used as-is), or a {@link DeploymentManifestStore} it loads from once and
 * caches.
 */
export type DeploymentManifestSource = DeploymentManifest | DeploymentManifestStore

/**
 * Map a runtime remote's federation `name`/`alias` to a deployment-manifest remote
 * `name`. Defaults to using the remote's `alias` if present, else its `name`.
 */
export type RemoteNameResolver = (remote: FederationRuntimeRemote) => string

/** Options for {@link createTreatyDeploymentRuntimePlugin}. */
export interface TreatyDeploymentRuntimePluginOptions {
	/** Plugin name reported to the host. Defaults to `'treaty-deployment'`. */
	readonly name?: string
	/** Map a runtime remote to its deployment-manifest remote name. */
	readonly remoteNameFor?: RemoteNameResolver
	/**
	 * Leave a remote untouched (default) when the manifest has no entry for it,
	 * instead of throwing. Set `false` to fail fast on an unknown remote.
	 */
	readonly passthroughUnknown?: boolean
}

/**
 * The factory `@module-federation/enhanced/runtime` expects: a zero-arg function
 * returning a plugin object, suitable for `registerPlugins` / `init({ plugins })`.
 */
export type TreatyDeploymentRuntimePluginFactory = () => FederationRuntimePlugin

const DEFAULT_PLUGIN_NAME = 'treaty-deployment'

/**
 * Build a `@module-federation/enhanced` runtime plugin that resolves every
 * remote's CURRENT version+entry from a {@link DeploymentManifest} at load time.
 *
 * Pass the manifest directly, or a {@link DeploymentManifestStore} the plugin
 * loads from once (then caches). The returned factory yields a plugin whose
 * `beforeRequest`/`resolveRemote` hooks rewrite each remote's `entry` and stamp its
 * `version` from the ledger. Because resolution happens at load, a deploy/rollback
 * that flips a remote's `currentVersion` is reflected on the next load with no
 * rebuild.
 */
export function createTreatyDeploymentRuntimePlugin(
	source: DeploymentManifestSource,
	options: TreatyDeploymentRuntimePluginOptions = {}
): TreatyDeploymentRuntimePluginFactory {
	const name = options.name ?? DEFAULT_PLUGIN_NAME
	const remoteNameFor = options.remoteNameFor ?? defaultRemoteNameFor
	const passthroughUnknown = options.passthroughUnknown ?? true

	let cached: DeploymentManifest | undefined = isStore(source) ? undefined : source
	let inflight: Promise<DeploymentManifest> | undefined

	const loadManifest = async (): Promise<DeploymentManifest> => {
		if (cached) return cached
		if (!inflight) {
			inflight = Promise.resolve((source as DeploymentManifestStore).load()).then((m) => {
				if (!m) {
					throw new RangeError(
						`[${name}] deployment manifest store returned no manifest`
					)
				}
				cached = m
				return m
			})
		}
		return inflight
	}

	const applyToRemote = (
		remote: FederationRuntimeRemote,
		manifest: DeploymentManifest
	): FederationRuntimeRemote => {
		const remoteName = remoteNameFor(remote)
		const deployment: RemoteDeployment | undefined = manifest.remotes[remoteName]
		if (!deployment) {
			if (passthroughUnknown) return remote
			throw new RangeError(
				`[${name}] no deployment manifest entry for remote "${remoteName}" (remote "${remote.name}")`
			)
		}
		return { ...remote, entry: deployment.entry, version: deployment.currentVersion }
	}

	return () => ({
		name,

		async beforeRequest(args: FederationBeforeRequestArgs): Promise<FederationBeforeRequestArgs> {
			const manifest = await loadManifest()
			const remotes = args.options.remotes.map((r) => applyToRemote(r, manifest))
			return { ...args, options: { ...args.options, remotes } }
		},

		async resolveRemote(args: FederationResolveRemoteArgs): Promise<FederationRuntimeRemote> {
			const manifest = await loadManifest()
			return applyToRemote(args.remote, manifest)
		},
	})
}

/** Default {@link RemoteNameResolver}: prefer the remote's `alias`, else its `name`. */
function defaultRemoteNameFor(remote: FederationRuntimeRemote): string {
	return remote.alias ?? remote.name
}

/** Structural check: is `source` a store (has a `load` method) vs a manifest object? */
function isStore(source: DeploymentManifestSource): source is DeploymentManifestStore {
	return typeof (source as DeploymentManifestStore).load === 'function'
}
