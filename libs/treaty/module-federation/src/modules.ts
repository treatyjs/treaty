/**
 * @module
 *
 * The **route-graph → deployable-module** pass: the bridge from Treaty's
 * auto-derived federation `exposes` (every lazy route + every lib, with no
 * hand-written exposes — see {@link generateMfConfig}) to the set of
 * independently versioned, deployable, rollback-able **modules** the deployment
 * layer (`@treaty/federation-deploy`) versions in its manifest.
 *
 * Federation is Treaty's unit of **deployment granularity**, and that granularity
 * is exactly this module's output: the host container plus one descriptor per
 * lazy feature route and per library. Where {@link deriveExposesFromRoutes} /
 * {@link deriveExposesFromLibs} answer *"what does the bundler expose?"*, this
 * module answers *"what does the deploy layer version, deploy, and roll back?"* —
 * and it answers it from the same route graph, so the two never drift.
 *
 * {@link federatedModules} runs {@link generateMfConfig} to get the auto-derived
 * exposes, then turns each exposed key into a {@link FederatedModule} carrying its
 * stable `moduleId` (the exposes key — the same identity the manifest uses), its
 * `kind` (host / route / lib), and the local module `path` it is backed by. The
 * host itself is always included as the `host` module. The result drops straight
 * into a versioned manifest: each descriptor maps 1:1 onto a `FederatedModuleInput`
 * (`{ moduleId, version, url, kind }`) via {@link toFederatedModuleInputs}.
 *
 * This package owns the route-graph pass but takes no dependency on the deploy
 * layer, so the descriptor + input types are declared locally and are structurally
 * identical to `@treaty/federation-deploy`'s `ModuleKind` / `FederatedModuleInput`.
 */

import { generateMfConfig, type MfOptions } from './config.js'
import {
	DEFAULT_LIB_KEY_PREFIX,
	DEFAULT_ROUTE_KEY_PREFIX,
} from './routes.js'

/**
 * The classification of a federated module — the host container, a lazy feature
 * route, or a shared library. Structurally identical to
 * `@treaty/federation-deploy`'s `ModuleKind`, declared here so this package needs
 * no dependency on the deploy layer.
 */
export type FederatedModuleKind = 'host' | 'route' | 'lib'

/**
 * One independently deployable federated module derived from the route graph: its
 * stable `moduleId` (the federation exposes key for a route/lib, or the host name
 * for the host), what `kind` of module it is, and the local module `path` that
 * backs it (the exposed source path; the host's path is its filename). The
 * `moduleId` is the identity the deployment manifest versions, deploys, and rolls
 * back — the same key the runtime resolves a remote against.
 */
export interface FederatedModule {
	/** Stable identity across versions: the exposes key (route/lib) or the host name. */
	readonly moduleId: string
	/** Whether this module is the host container, a lazy route, or a library. */
	readonly kind: FederatedModuleKind
	/** Local module path backing the module (exposed source path; host's filename). */
	readonly path: string
}

/**
 * A federated module seeded for a versioned manifest — the shape
 * `@treaty/federation-deploy`'s `buildManifest` consumes (`FederatedModuleInput`).
 * Declared locally to keep this package dependency-free; identical in shape so the
 * value passes straight to `buildManifest`.
 */
export interface FederatedModuleInput {
	/** Stable identity of the module across all of its versions. */
	readonly moduleId: string
	/** The version to make live for this module. */
	readonly version: string
	/** URL the module's remote entry is served from. */
	readonly url: string
	/** What kind of module this is. */
	readonly kind: FederatedModuleKind
}

/** Options for {@link federatedModules}. */
export interface FederatedModulesOptions {
	/**
	 * Include the host container as a `host` module in the result. Defaults to
	 * `true` — the host is itself a versioned, deployable federated module (it can
	 * be rolled back like any remote). Set `false` to enumerate only the
	 * exposed route/lib remotes.
	 */
	readonly includeHost?: boolean
}

/**
 * Classify a federation exposes key into a {@link FederatedModuleKind}. Keys under
 * the lib prefix (`./libs/…`) are libraries; everything else (route keys under
 * `./routes/…`, or any manual expose) is a route remote.
 */
function kindForExposeKey(key: string): FederatedModuleKind {
	if (key === DEFAULT_LIB_KEY_PREFIX || key.startsWith(`${DEFAULT_LIB_KEY_PREFIX}/`)) {
		return 'lib'
	}
	return 'route'
}

/**
 * Derive the set of independently deployable federated **modules** from a Treaty
 * app's federation options — the route-graph-to-deployment-unit pass.
 *
 * Runs the same auto-derivation Treaty wires at build time (every lazy
 * `loadComponent`/`loadChildren` route and every lib becomes an exposed remote,
 * with manual {@link MfOptions.exposes} still winning), then returns one
 * {@link FederatedModule} per exposed key plus — by default — the host. Each
 * module's `moduleId` is the exposes key (the same identity the deployment
 * manifest versions), classified `route` or `lib` by its prefix; the host is
 * `host`. Modules are returned sorted by `moduleId` for deterministic output.
 *
 * When federation is **disabled** ({@link MfOptions.enabled} `=== false`),
 * {@link generateMfConfig} yields an inert config (no exposes); this returns just
 * the host (or nothing, when `includeHost` is `false`) — there are no federated
 * route/lib remotes to deploy.
 *
 * This is the input to the versioned-manifest layer: map the result through
 * {@link toFederatedModuleInputs} and hand it to `buildManifest`.
 *
 * @example
 * federatedModules({
 *   name: 'shell',
 *   routes: [
 *     { path: 'dashboard', loadComponent: () => import('./dashboard') },
 *     { path: 'home', component: HomeComponent }, // eager: not a module
 *   ],
 *   libs: ['./libs/data-access'],
 * })
 * // => [
 * //   { moduleId: './libs/data-access', kind: 'lib',  path: './libs/data-access' },
 * //   { moduleId: './routes/dashboard', kind: 'route', path: './src/app/dashboard' },
 * //   { moduleId: 'shell',              kind: 'host', path: 'remoteEntry.js' },
 * // ]
 */
export function federatedModules(
	options: MfOptions = {},
	modulesOptions: FederatedModulesOptions = {}
): FederatedModule[] {
	const config = generateMfConfig(options)
	const includeHost = modulesOptions.includeHost ?? true

	const modules: FederatedModule[] = []
	if (includeHost) {
		modules.push({ moduleId: config.name, kind: 'host', path: config.filename })
	}
	for (const [key, path] of Object.entries(config.exposes)) {
		modules.push({ moduleId: key, kind: kindForExposeKey(key), path })
	}

	modules.sort((a, b) => (a.moduleId < b.moduleId ? -1 : a.moduleId > b.moduleId ? 1 : 0))
	return modules
}

/** Options for {@link toFederatedModuleInputs}. */
export interface ToFederatedModuleInputsOptions {
	/**
	 * The version to stamp on every module. Defaults to `'0.0.0'` — the placeholder
	 * a first deploy seeds before any real version exists. Pass a constant build id
	 * or per-module versions (assign them on the result) for a real release.
	 */
	readonly version?: string
	/**
	 * Build the served url for a module at a version. Defaults to a relative,
	 * version-stamped path `"<moduleId>/<version>/remoteEntry.js"` — the deploy
	 * target rewrites it to an absolute url when the artifact is published.
	 */
	readonly urlFor?: (module: FederatedModule, version: string) => string
}

const DEFAULT_VERSION = '0.0.0'
const DEFAULT_ENTRY = 'remoteEntry.js'

/**
 * Turn {@link FederatedModule} descriptors into the
 * {@link FederatedModuleInput}s a versioned manifest is built from — the final
 * step of the route-graph → manifest bridge.
 *
 * Each descriptor keeps its `moduleId` and `kind` and gains a `version` (default
 * `'0.0.0'`, a first-deploy placeholder) and a derived `url` (default a relative,
 * version-stamped path the deploy target later rewrites). The result is exactly
 * what `@treaty/federation-deploy`'s `buildManifest` accepts, so a route graph
 * seeds a versioned manifest with no manual module list.
 */
export function toFederatedModuleInputs(
	modules: readonly FederatedModule[],
	options: ToFederatedModuleInputsOptions = {}
): FederatedModuleInput[] {
	const version = options.version ?? DEFAULT_VERSION
	const urlFor = options.urlFor ?? ((m, v) => `${m.moduleId}/${v}/${DEFAULT_ENTRY}`)

	return modules.map((module) => ({
		moduleId: module.moduleId,
		version,
		url: urlFor(module, version),
		kind: module.kind,
	}))
}

/**
 * Convenience: derive the federated {@link FederatedModuleInput}s directly from a
 * Treaty app's federation {@link MfOptions} — {@link federatedModules} followed by
 * {@link toFederatedModuleInputs} in one call. The route graph in, the versioned
 * manifest's module seed out.
 */
export function federatedModuleInputs(
	options: MfOptions = {},
	inputsOptions: ToFederatedModuleInputsOptions & FederatedModulesOptions = {}
): FederatedModuleInput[] {
	const { includeHost, ...toInputs } = inputsOptions
	return toFederatedModuleInputs(
		federatedModules(options, includeHost !== undefined ? { includeHost } : {}),
		toInputs
	)
}

/** Re-export the route/lib key prefixes so callers can build matching expose keys. */
export { DEFAULT_ROUTE_KEY_PREFIX, DEFAULT_LIB_KEY_PREFIX }
