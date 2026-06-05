/**
 * @module
 *
 * The versioned federation manifest — Treaty's deployment ledger. Federation is
 * Treaty's unit of **deployment granularity**: the compiler emits the host, each
 * lazy feature route, and each library as an independently built federated
 * module. This manifest is the runtime-facing record that says, for every one of
 * those `moduleId`s, which **version** is live and at which **url** it is served.
 *
 * Treaty is a compiler, not a host: it produces modules + this manifest. The
 * serving platform owns the manifest and flips entries. Two operations express
 * the whole deployment lifecycle, and both are *pure* — they return a NEW
 * manifest with exactly one module repointed and every other entry untouched:
 *   - {@link setModuleVersion} — **deploy**: point one module at a new version/url.
 *   - {@link rollbackModule} — **rollback**: revert one module to a prior version.
 *
 * Because each module is versioned independently you can deploy or roll back a
 * single route/lib without rebuilding or redeploying the rest of the app. The
 * runtime ({@link createTreatyMfRuntimePlugin}) reads the manifest at load and
 * resolves each remote to its current url+version.
 */

/**
 * One federated module's live deployment state: the version currently served and
 * the url it is served from. `url` is the remote's entry manifest
 * (`remoteEntry.js` / `mf-manifest.json`); typically version-stamped so a
 * rollback repoints to an immutable prior artifact rather than mutating in place.
 */
export interface ModuleDeployment {
	/** The version string currently live for this module (e.g. a semver or build id). */
	readonly version: string
	/** URL the module's remote entry is served from for {@link ModuleDeployment.version}. */
	readonly url: string
}

/** The kind of federated module a manifest entry describes. */
export type ModuleKind = 'host' | 'route' | 'lib'

/**
 * A federated module to seed into a manifest: its stable `moduleId`, the
 * `version` to make live, the `url` that version is served from, and optionally
 * what `kind` of module it is (host / route remote / lib). The `moduleId` is the
 * stable identity that survives across versions — it is what deploy/rollback
 * address and what the runtime resolves.
 */
export interface FederatedModuleInput {
	/** Stable identity of the module across all of its versions. */
	readonly moduleId: string
	/** The version to make live for this module. */
	readonly version: string
	/** URL the module's remote entry is served from. */
	readonly url: string
	/** What kind of module this is. Defaults to `'route'` when omitted. */
	readonly kind?: ModuleKind
}

/**
 * The versioned federation manifest: every federated `moduleId` (the host, each
 * route remote, each lib) mapped to its current {@link ModuleDeployment}. This is
 * the single source of truth the platform serves and the runtime resolves
 * against. Treated as immutable — the deploy/rollback helpers return new copies.
 */
export interface FederationManifest {
	/** Manifest schema version, so consumers can evolve the shape safely. */
	readonly schema: 1
	/**
	 * Optional name of the app this manifest describes. Purely informational;
	 * resolution keys off {@link FederationManifest.modules}.
	 */
	readonly app?: string
	/** Live deployment of every federated module, keyed by `moduleId`. */
	readonly modules: Readonly<Record<string, ModuleDeployment>>
	/** Optional per-module classification, keyed by `moduleId`. */
	readonly kinds?: Readonly<Record<string, ModuleKind>>
}

/** Current manifest schema version emitted by {@link buildManifest}. */
export const MANIFEST_SCHEMA = 1 as const

/** Options for {@link buildManifest}. */
export interface BuildManifestOptions {
	/** Optional informational app name recorded on the manifest. */
	readonly app?: string
}

/**
 * Build a {@link FederationManifest} from the federated modules Treaty emitted.
 * Each input contributes one `moduleId -> { version, url }` entry; a later input
 * with the same `moduleId` wins (last-write-wins) so callers can layer overrides.
 * The result is frozen — callers mutate it only through {@link setModuleVersion}
 * / {@link rollbackModule}, which return fresh manifests.
 *
 * @throws if any input has an empty `moduleId`, `version`, or `url`.
 */
export function buildManifest(
	modules: Iterable<FederatedModuleInput>,
	options: BuildManifestOptions = {}
): FederationManifest {
	const out: Record<string, ModuleDeployment> = {}
	const kinds: Record<string, ModuleKind> = {}

	for (const mod of modules) {
		if (!mod || typeof mod.moduleId !== 'string' || mod.moduleId.length === 0) {
			throw new TypeError('buildManifest: every module needs a non-empty moduleId')
		}
		if (typeof mod.version !== 'string' || mod.version.length === 0) {
			throw new TypeError(`buildManifest: module "${mod.moduleId}" needs a non-empty version`)
		}
		if (typeof mod.url !== 'string' || mod.url.length === 0) {
			throw new TypeError(`buildManifest: module "${mod.moduleId}" needs a non-empty url`)
		}
		out[mod.moduleId] = { version: mod.version, url: mod.url }
		kinds[mod.moduleId] = mod.kind ?? 'route'
	}

	const manifest: FederationManifest = {
		schema: MANIFEST_SCHEMA,
		...(options.app !== undefined ? { app: options.app } : {}),
		modules: out,
		kinds,
	}
	return freezeManifest(manifest)
}

/**
 * Serialize a manifest to canonical JSON. Module keys are emitted in sorted
 * order so the output is stable/diffable across builds (the manifest is a
 * deploy artifact that gets committed/compared). `space` controls indentation
 * (default `2`); pass `0` for a compact single line.
 */
export function serializeManifest(manifest: FederationManifest, space: number = 2): string {
	const sortedModules: Record<string, ModuleDeployment> = {}
	for (const id of Object.keys(manifest.modules).sort()) {
		const dep = manifest.modules[id]!
		sortedModules[id] = { version: dep.version, url: dep.url }
	}

	const sortedKinds: Record<string, ModuleKind> = {}
	if (manifest.kinds) {
		for (const id of Object.keys(manifest.kinds).sort()) {
			sortedKinds[id] = manifest.kinds[id]!
		}
	}

	const ordered = {
		schema: manifest.schema,
		...(manifest.app !== undefined ? { app: manifest.app } : {}),
		modules: sortedModules,
		...(manifest.kinds ? { kinds: sortedKinds } : {}),
	}
	return JSON.stringify(ordered, null, space)
}

/**
 * Parse a manifest from the JSON produced by {@link serializeManifest},
 * validating its shape. The result is frozen, so `parseManifest(serializeManifest(m))`
 * round-trips to a value deep-equal to `m`.
 *
 * @throws if the JSON is malformed or does not match the manifest schema.
 */
export function parseManifest(json: string): FederationManifest {
	let raw: unknown
	try {
		raw = JSON.parse(json)
	} catch (err) {
		throw new SyntaxError(`parseManifest: invalid JSON: ${(err as Error).message}`)
	}
	if (typeof raw !== 'object' || raw === null) {
		throw new TypeError('parseManifest: manifest must be a JSON object')
	}

	const obj = raw as Record<string, unknown>
	if (obj['schema'] !== MANIFEST_SCHEMA) {
		throw new TypeError(`parseManifest: unsupported manifest schema (expected ${MANIFEST_SCHEMA})`)
	}
	if (typeof obj['modules'] !== 'object' || obj['modules'] === null) {
		throw new TypeError('parseManifest: manifest is missing a "modules" object')
	}

	const modules: Record<string, ModuleDeployment> = {}
	for (const [id, value] of Object.entries(obj['modules'] as Record<string, unknown>)) {
		if (typeof value !== 'object' || value === null) {
			throw new TypeError(`parseManifest: module "${id}" must be an object`)
		}
		const dep = value as Record<string, unknown>
		if (typeof dep['version'] !== 'string' || dep['version'].length === 0) {
			throw new TypeError(`parseManifest: module "${id}" needs a non-empty version`)
		}
		if (typeof dep['url'] !== 'string' || dep['url'].length === 0) {
			throw new TypeError(`parseManifest: module "${id}" needs a non-empty url`)
		}
		modules[id] = { version: dep['version'], url: dep['url'] }
	}

	let kinds: Record<string, ModuleKind> | undefined
	if (obj['kinds'] !== undefined) {
		if (typeof obj['kinds'] !== 'object' || obj['kinds'] === null) {
			throw new TypeError('parseManifest: "kinds" must be an object when present')
		}
		kinds = {}
		for (const [id, value] of Object.entries(obj['kinds'] as Record<string, unknown>)) {
			if (value !== 'host' && value !== 'route' && value !== 'lib') {
				throw new TypeError(`parseManifest: kind for "${id}" must be host|route|lib`)
			}
			kinds[id] = value
		}
	}

	const manifest: FederationManifest = {
		schema: MANIFEST_SCHEMA,
		...(typeof obj['app'] === 'string' ? { app: obj['app'] } : {}),
		modules,
		...(kinds ? { kinds } : {}),
	}
	return freezeManifest(manifest)
}

/**
 * Look up a module's current deployment, or `undefined` if the manifest does not
 * carry that `moduleId`.
 */
export function getModule(manifest: FederationManifest, moduleId: string): ModuleDeployment | undefined {
	return manifest.modules[moduleId]
}

/** Options for {@link setModuleVersion}. */
export interface SetModuleVersionOptions {
	/**
	 * The url the new version is served from. Required unless a
	 * {@link SetModuleVersionOptions.urlFor} resolver is supplied — there is no
	 * implicit "keep the old url" because versioned artifacts are version-stamped.
	 */
	readonly url?: string
	/**
	 * Derive the url for the new version from the existing deployment, e.g. to
	 * swap a version segment in a stamped url. Used when `url` is omitted.
	 */
	readonly urlFor?: (version: string, previous: ModuleDeployment) => string
}

/**
 * **Deploy**: return a NEW manifest with a single module repointed to `version`
 * (and its new `url`), leaving every other module exactly as it was. This is the
 * primitive the platform calls to roll a new version of one route/lib forward
 * without redeploying the app.
 *
 * The url for the new version is resolved from `options.url`, else from
 * `options.urlFor(version, previous)`. The input manifest is never mutated.
 *
 * @throws if the module is unknown, or if no url can be resolved.
 */
export function setModuleVersion(
	manifest: FederationManifest,
	moduleId: string,
	version: string,
	options: SetModuleVersionOptions = {}
): FederationManifest {
	const previous = manifest.modules[moduleId]
	if (!previous) {
		throw new RangeError(`setModuleVersion: unknown moduleId "${moduleId}"`)
	}
	if (typeof version !== 'string' || version.length === 0) {
		throw new TypeError(`setModuleVersion: "${moduleId}" needs a non-empty version`)
	}

	const url =
		options.url ?? (options.urlFor ? options.urlFor(version, previous) : undefined)
	if (typeof url !== 'string' || url.length === 0) {
		throw new TypeError(
			`setModuleVersion: "${moduleId}" needs a url for version "${version}" (pass options.url or options.urlFor)`
		)
	}

	return repoint(manifest, moduleId, { version, url })
}

/**
 * **Rollback**: return a NEW manifest with a single module reverted to a prior
 * `toVersion`, leaving every other module untouched. This is the inverse of a
 * deploy and the reason every module is versioned independently — you can revert
 * one bad route/lib without disturbing the rest of the live app.
 *
 * The url for the target version is resolved from `options.url`, else from
 * `options.urlFor(toVersion, previous)`. The input manifest is never mutated.
 *
 * @throws if the module is unknown, or if no url can be resolved.
 */
export function rollbackModule(
	manifest: FederationManifest,
	moduleId: string,
	toVersion: string,
	options: SetModuleVersionOptions = {}
): FederationManifest {
	const previous = manifest.modules[moduleId]
	if (!previous) {
		throw new RangeError(`rollbackModule: unknown moduleId "${moduleId}"`)
	}
	if (typeof toVersion !== 'string' || toVersion.length === 0) {
		throw new TypeError(`rollbackModule: "${moduleId}" needs a non-empty target version`)
	}

	const url =
		options.url ?? (options.urlFor ? options.urlFor(toVersion, previous) : undefined)
	if (typeof url !== 'string' || url.length === 0) {
		throw new TypeError(
			`rollbackModule: "${moduleId}" needs a url for version "${toVersion}" (pass options.url or options.urlFor)`
		)
	}

	return repoint(manifest, moduleId, { version: toVersion, url })
}

/**
 * Return a new manifest identical to `manifest` except that `moduleId` points at
 * `deployment`. Shared internal of {@link setModuleVersion}/{@link rollbackModule};
 * the single place that copies the modules map so exactly one entry changes.
 */
function repoint(
	manifest: FederationManifest,
	moduleId: string,
	deployment: ModuleDeployment
): FederationManifest {
	const modules: Record<string, ModuleDeployment> = {}
	for (const [id, dep] of Object.entries(manifest.modules)) {
		modules[id] = id === moduleId ? { version: deployment.version, url: deployment.url } : dep
	}

	const next: FederationManifest = {
		schema: manifest.schema,
		...(manifest.app !== undefined ? { app: manifest.app } : {}),
		modules,
		...(manifest.kinds ? { kinds: { ...manifest.kinds } } : {}),
	}
	return freezeManifest(next)
}

/** Deep-freeze a manifest so it cannot be mutated in place. */
function freezeManifest(manifest: FederationManifest): FederationManifest {
	for (const dep of Object.values(manifest.modules)) Object.freeze(dep)
	Object.freeze(manifest.modules)
	if (manifest.kinds) Object.freeze(manifest.kinds)
	return Object.freeze(manifest)
}
