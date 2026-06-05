/**
 * @module
 *
 * The runtime side of Treaty's federation deployment layer. The build emits a
 * {@link FederationManifest} (see {@link buildManifest}); the serving platform
 * keeps it current with {@link setModuleVersion} (deploy) and
 * {@link rollbackModule} (rollback). At load time the host must resolve each
 * remote to the url+version the manifest currently points at — so a flipped
 * manifest takes effect on the next load with no rebuild.
 *
 * {@link createTreatyMfRuntimePlugin} produces a `@module-federation/enhanced`
 * runtime plugin (a function returning a named plugin object) that does exactly
 * that: in `beforeRequest` it rewrites each requested remote's entry url from the
 * manifest, and in `resolveRemote` it repoints a remote definition by name. The
 * manifest may be passed as an object (resolved eagerly) or as a url string
 * (fetched lazily on first use and cached). The `@module-federation/enhanced/runtime`
 * peer is referenced **structurally** so this module typechecks without it.
 */

import type {
	FederationBeforeRequestArgs,
	FederationResolveRemoteArgs,
	FederationRuntimePlugin,
	FederationRuntimeRemote,
} from '@module-federation/enhanced/runtime'
import { parseManifest, type FederationManifest, type ModuleDeployment } from './manifest.js'

/** The two ways to give the plugin its manifest: an object, or a url to fetch. */
export type ManifestSource = FederationManifest | string

/**
 * How a remote's federation `name` (or `alias`) maps to a manifest `moduleId`.
 * Defaults to identity — Treaty names remotes after their moduleId. Provide this
 * when the runtime remote name differs from the manifest key.
 */
export type ModuleIdResolver = (remote: FederationRuntimeRemote) => string

/** Options for {@link createTreatyMfRuntimePlugin}. */
export interface TreatyMfRuntimePluginOptions {
	/** Plugin name reported to the host. Defaults to `'treaty-federation-deploy'`. */
	readonly name?: string
	/**
	 * Map a runtime remote to its manifest `moduleId`. Defaults to using the
	 * remote's `alias` if present, else its `name`.
	 */
	readonly moduleIdFor?: ModuleIdResolver
	/**
	 * Custom fetcher for the string-url manifest source, returning the manifest
	 * JSON text. Defaults to `globalThis.fetch`. Lets non-browser hosts (or tests)
	 * supply their own transport.
	 */
	readonly fetchManifest?: (url: string) => Promise<string>
	/**
	 * When the manifest has no entry for a resolved `moduleId`, leave the remote
	 * untouched (default) instead of throwing. Set `false` to fail fast on a
	 * remote the manifest does not know about.
	 */
	readonly passthroughUnknown?: boolean
}

/**
 * The factory `@module-federation/enhanced/runtime` expects: a zero-arg function
 * returning a plugin object. This is the value you pass to `registerPlugins`
 * (or `init({ plugins: [createTreatyMfRuntimePlugin(...)] })`).
 */
export type TreatyMfRuntimePluginFactory = () => FederationRuntimePlugin

const DEFAULT_PLUGIN_NAME = 'treaty-federation-deploy'

/**
 * Build a `@module-federation/enhanced` runtime plugin that resolves every
 * remote's CURRENT url+version from a {@link FederationManifest} at load time.
 *
 * Pass the manifest as an object (used as-is) or as a url string (fetched once on
 * first use, then cached). The returned value is the factory the host registers;
 * calling it yields a plugin whose `beforeRequest`/`resolveRemote` hooks rewrite
 * each remote's `entry` (and stamp `version`) from the manifest. Because the
 * lookup happens at load, flipping the manifest via {@link setModuleVersion} /
 * {@link rollbackModule} repoints the next load with no rebuild.
 *
 * @param source The manifest object, or a url to fetch the manifest JSON from.
 * @param options See {@link TreatyMfRuntimePluginOptions}.
 */
export function createTreatyMfRuntimePlugin(
	source: ManifestSource,
	options: TreatyMfRuntimePluginOptions = {}
): TreatyMfRuntimePluginFactory {
	const name = options.name ?? DEFAULT_PLUGIN_NAME
	const moduleIdFor = options.moduleIdFor ?? defaultModuleIdFor
	const passthroughUnknown = options.passthroughUnknown ?? true

	// Resolve the manifest once; cache the in-flight promise so concurrent loads
	// share a single fetch+parse.
	let cached: FederationManifest | undefined =
		typeof source === 'string' ? undefined : source
	let inflight: Promise<FederationManifest> | undefined

	const loadManifest = async (): Promise<FederationManifest> => {
		if (cached) return cached
		if (!inflight) {
			inflight = fetchAndParse(source as string, options.fetchManifest).then((m) => {
				cached = m
				return m
			})
		}
		return inflight
	}

	/**
	 * Rewrite a single remote from its manifest deployment: point `entry` at the
	 * current url and stamp `version`. Returns a new remote object; never mutates
	 * the input. Unknown modules pass through (or throw, per `passthroughUnknown`).
	 */
	const applyToRemote = (
		remote: FederationRuntimeRemote,
		manifest: FederationManifest
	): FederationRuntimeRemote => {
		const moduleId = moduleIdFor(remote)
		const deployment: ModuleDeployment | undefined = manifest.modules[moduleId]
		if (!deployment) {
			if (passthroughUnknown) return remote
			throw new RangeError(
				`[${name}] no manifest entry for module "${moduleId}" (remote "${remote.name}")`
			)
		}
		return { ...remote, entry: deployment.url, version: deployment.version }
	}

	return () => ({
		name,

		// Before a module is requested, repoint every candidate remote's entry to
		// the manifest's current url+version. `beforeRequest` runs per request, so
		// a manifest flip is reflected on the next load.
		async beforeRequest(args: FederationBeforeRequestArgs): Promise<FederationBeforeRequestArgs> {
			const manifest = await loadManifest()
			const remotes = args.options.remotes.map((r) => applyToRemote(r, manifest))
			return { ...args, options: { ...args.options, remotes } }
		},

		// When the host resolves a remote by name, hand back the manifest-pinned
		// definition so its url+version reflect the live deployment.
		async resolveRemote(args: FederationResolveRemoteArgs): Promise<FederationRuntimeRemote> {
			const manifest = await loadManifest()
			return applyToRemote(args.remote, manifest)
		},
	})
}

/** Default {@link ModuleIdResolver}: prefer the remote's `alias`, else its `name`. */
function defaultModuleIdFor(remote: FederationRuntimeRemote): string {
	return remote.alias ?? remote.name
}

/** Fetch a manifest url to text (via the supplied or global `fetch`) and parse it. */
async function fetchAndParse(
	url: string,
	fetchManifest?: (url: string) => Promise<string>
): Promise<FederationManifest> {
	if (fetchManifest) {
		return parseManifest(await fetchManifest(url))
	}
	const fetchFn = (globalThis as { fetch?: (input: string) => Promise<{ text(): Promise<string> }> })
		.fetch
	if (!fetchFn) {
		throw new TypeError(
			`createTreatyMfRuntimePlugin: no fetch available to load manifest from "${url}" (pass options.fetchManifest)`
		)
	}
	const response = await fetchFn(url)
	return parseManifest(await response.text())
}
