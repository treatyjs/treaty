/**
 * @module
 *
 * The federation **on/off toggle** as the deploy pipeline sees it, plus the
 * bridge from an **ejected** standalone federation config to the deploy layer's
 * module set.
 *
 * Federation is on by default in Treaty; a `federation: false` (or
 * `{ enabled: false }`) in the Treaty config turns auto-MF off. The compiler-side
 * reader of that toggle lives in `@treaty/module-federation`
 * (`resolveFederation`). This module re-states the toggle shape locally — so
 * `@treaty/federation-deploy` stays free of a hard dependency on the
 * module-federation package — and exposes {@link isFederationEnabled} so a CI
 * deploy run can **skip entirely** when federation is off (no manifest, no plan,
 * no deploy).
 *
 * It also closes the loop on eject: a user who has ejected a standalone
 * `@module-federation/enhanced` config can feed it back in.
 * {@link federatedModuleIdsFromConfig} reads the host + every `exposes` key out
 * of that config as deploy `moduleId`s, and {@link manifestModulesFromConfig}
 * turns them into {@link FederatedModuleInput}s ready for {@link buildManifest} —
 * so the ejected config drives the versioned manifest without Treaty re-deriving
 * anything.
 */

import type { FederatedModuleInput, ModuleKind } from './manifest.js'

/**
 * The federation toggle a Treaty config carries, mirrored from
 * `@treaty/module-federation`'s `FederationConfig` so this package needs no
 * dependency on it. `false` / `{ enabled: false }` ⇒ off; everything else ⇒ on.
 */
export type FederationToggle = boolean | { readonly enabled?: boolean; readonly [extra: string]: unknown }

/**
 * Whether federation is **on** for the given toggle. The deploy pipeline calls
 * this to decide whether to run at all:
 *   - `undefined` / `true` / any object without `enabled: false` ⇒ `true` (on).
 *   - `false` / `{ enabled: false }` ⇒ `false` (off — skip the deploy run).
 *
 * @example
 * if (!isFederationEnabled(treatyConfig.federation)) return // nothing to deploy
 */
export function isFederationEnabled(toggle: FederationToggle | undefined): boolean {
	if (toggle === false) return false
	if (toggle === true || toggle === undefined) return true
	return toggle.enabled !== false
}

/**
 * The subset of an ejected standalone `@module-federation/enhanced` config the
 * deploy layer reads. Matches `StandaloneFederationConfig` from
 * `@treaty/module-federation` structurally (host `name`/`filename` plus the
 * `exposes` map), declared locally so this package stays dependency-free. Extra
 * fields (`remotes`, `shared`, …) are accepted and ignored.
 */
export interface EjectedFederationConfig {
	/** The host container/library name. */
	readonly name: string
	/** The host's own remote-entry filename (informational here). */
	readonly filename?: string
	/** Exposed modules, keyed by public path → local module path. */
	readonly exposes?: Readonly<Record<string, string>>
	/** Allow (and ignore) the rest of an enhanced config (`remotes`, `shared`, …). */
	readonly [extra: string]: unknown
}

/** Classify an exposes key into a manifest {@link ModuleKind}. */
function kindForExposeKey(key: string): ModuleKind {
	if (key.startsWith('./libs/') || key === './libs') return 'lib'
	return 'route'
}

/**
 * Read the federated **module ids** out of an ejected standalone config: the host
 * (its `name`) plus every `exposes` key (each a lazy route or lib that is an
 * independently deployable remote). Returned sorted and de-duplicated.
 *
 * This is the inverse direction of eject — given the config a user owns, recover
 * the set of modules the deploy layer versions and ships.
 */
export function federatedModuleIdsFromConfig(config: EjectedFederationConfig): string[] {
	const ids = new Set<string>()
	if (typeof config.name === 'string' && config.name.length > 0) ids.add(config.name)
	for (const key of Object.keys(config.exposes ?? {})) ids.add(key)
	return [...ids].sort()
}

/** Options for {@link manifestModulesFromConfig}. */
export interface ManifestModulesFromConfigOptions {
	/** The version to stamp on every module. Defaults to `'0.0.0'` (a placeholder first deploy). */
	readonly version?: string
	/**
	 * Build the served url for a module id at a version. Defaults to
	 * `<moduleId>/<version>/remoteEntry.js` (a relative, version-stamped path the
	 * deploy target rewrites to an absolute url).
	 */
	readonly urlFor?: (moduleId: string, version: string) => string
}

const DEFAULT_VERSION = '0.0.0'
const DEFAULT_ENTRY = 'remoteEntry.js'

/**
 * Turn an ejected standalone config into {@link FederatedModuleInput}s ready to
 * hand to {@link buildManifest}. The host (`name`) is classified `host`; every
 * `exposes` key under `./libs/` is a `lib` and the rest are `route`s. Each gets
 * the supplied version and a derived url, so an ejected config seeds an initial
 * versioned manifest with no further Treaty involvement.
 */
export function manifestModulesFromConfig(
	config: EjectedFederationConfig,
	options: ManifestModulesFromConfigOptions = {}
): FederatedModuleInput[] {
	const version = options.version ?? DEFAULT_VERSION
	const urlFor = options.urlFor ?? ((id, v) => `${id}/${v}/${DEFAULT_ENTRY}`)

	const inputs: FederatedModuleInput[] = []
	for (const moduleId of federatedModuleIdsFromConfig(config)) {
		const kind: ModuleKind = moduleId === config.name ? 'host' : kindForExposeKey(moduleId)
		inputs.push({ moduleId, version, url: urlFor(moduleId, version), kind })
	}
	return inputs
}
