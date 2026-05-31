/**
 * @module
 *
 * The **versioned deployment manifest** — Treaty's per-remote deployment ledger,
 * and the heart of federation as a unit of **deployment granularity**. Where the
 * {@link FederationManifest} (see `./manifest.ts`) is the lean runtime record of
 * *which version of each module is live right now*, the {@link DeploymentManifest}
 * is the operational ledger CI/CD maintains: for every federated **remote** (the
 * host, each lazy route, each lib) it records the version currently served, the
 * url it is served from, AND the **history** of every version that remote has had
 * deployed. That history is what makes a single remote independently
 * **rollback-able** — a rollback flips the served version back to a prior entry in
 * its history without rebuilding or redeploying anything else.
 *
 * Treaty is a compiler, not a host: it never owns where the manifest lives. So the
 * manifest is read and written through an injected {@link DeploymentManifestStore}
 * — an interface with `load()`/`save()` — letting the same deploy/rollback logic
 * run against a JSON file, an object-store key, a database row, or (in tests) an
 * in-memory map ({@link MemoryDeploymentStore}). The mutation helpers
 * ({@link recordDeployment}, {@link rollbackTo}) are *pure*: they take a manifest
 * and return a NEW one with exactly one remote changed and every other remote
 * untouched, so partial deploy/rollback never disturbs the rest of the app.
 */

/**
 * The classification of a federated remote a manifest entry describes — the host
 * container, a lazy feature route, or a shared library. Mirrors the manifest's
 * {@link ModuleKind} (re-exported here for callers that only touch the deployment
 * manifest).
 */
export type RemoteKind = 'host' | 'route' | 'lib'

/**
 * One federated remote's deployment ledger entry: its stable `name`, the
 * `currentVersion` served, the `entry` url that version is served from, and the
 * full `history` of versions deployed for it (oldest first, the current version
 * last). `history` is the set of versions a rollback may target — a still-served
 * prior artifact stays addressable because every version is published under its
 * own immutable, version-stamped url.
 */
export interface RemoteDeployment {
	/** Stable identity of the remote across all of its versions (the manifest key). */
	readonly name: string
	/** The version currently live/served for this remote. */
	readonly currentVersion: string
	/** URL the remote's entry is served from for {@link RemoteDeployment.currentVersion}. */
	readonly entry: string
	/**
	 * Every version this remote has had deployed, in deploy order (oldest first),
	 * de-duplicated, with {@link RemoteDeployment.currentVersion} always last. A
	 * rollback may only target a version present here.
	 */
	readonly history: readonly string[]
	/** What kind of remote this is (host / route / lib). */
	readonly kind: RemoteKind
}

/**
 * The versioned deployment manifest: every federated remote `name` mapped to its
 * {@link RemoteDeployment} ledger. This is the durable source of truth a
 * {@link DeploymentManifestStore} persists; the runtime resolves remotes against
 * it, and deploy/rollback flip individual entries. Treated as immutable — the
 * helpers return fresh copies.
 */
export interface DeploymentManifest {
	/** Manifest schema version, so consumers can evolve the shape safely. */
	readonly schema: 1
	/** Optional informational name of the app this manifest describes. */
	readonly app?: string
	/** Deployment ledger of every federated remote, keyed by remote `name`. */
	readonly remotes: Readonly<Record<string, RemoteDeployment>>
}

/** Current deployment-manifest schema version. */
export const DEPLOYMENT_MANIFEST_SCHEMA = 1 as const

/**
 * Persistence boundary for a {@link DeploymentManifest}. Treaty is a compiler, not
 * a host — it does not own where the ledger lives — so all reads/writes of the
 * manifest go through this injected interface. Implement it over a JSON file, an
 * object-store key, a database row, a config service, etc. {@link load} returns
 * `undefined` when no manifest has been persisted yet (a first deploy seeds one);
 * {@link save} durably writes the manifest, last-write-wins.
 */
export interface DeploymentManifestStore {
	/** Load the persisted manifest, or `undefined` if none has been written yet. */
	load(): Promise<DeploymentManifest | undefined> | DeploymentManifest | undefined
	/** Durably persist `manifest`, replacing any prior value. */
	save(manifest: DeploymentManifest): Promise<void> | void
}

/** Options for {@link createDeploymentManifest}. */
export interface CreateDeploymentManifestOptions {
	/** Optional informational app name recorded on the manifest. */
	readonly app?: string
}

/**
 * Create an empty (or app-named) deployment manifest with no remotes — the value a
 * {@link DeploymentManifestStore} starts from before the first deploy. Frozen;
 * mutate only through {@link recordDeployment} / {@link rollbackTo}.
 */
export function createDeploymentManifest(
	options: CreateDeploymentManifestOptions = {}
): DeploymentManifest {
	return freezeManifest({
		schema: DEPLOYMENT_MANIFEST_SCHEMA,
		...(options.app !== undefined ? { app: options.app } : {}),
		remotes: {},
	})
}

/** Look up a remote's current ledger entry, or `undefined` if absent. */
export function getRemote(manifest: DeploymentManifest, name: string): RemoteDeployment | undefined {
	return manifest.remotes[name]
}

/** Whether `manifest` carries a remote named `name`. */
export function hasRemote(manifest: DeploymentManifest, name: string): boolean {
	return name in manifest.remotes
}

/** Options for {@link recordDeployment}. */
export interface RecordDeploymentOptions {
	/** The remote's classification when first added. Defaults to `'route'`. */
	readonly kind?: RemoteKind
}

/**
 * **Deploy (ledger side)**: return a NEW manifest in which `name` is now serving
 * `version` from `entry`, with `version` appended to the remote's history, leaving
 * every other remote exactly as it was. If the remote is new, it is added with a
 * single-entry history. Re-recording the version already current is idempotent
 * (the url is refreshed but history does not grow). The input is never mutated.
 *
 * This is the manifest mutation a real deploy performs *after* the artifact has
 * been uploaded — see `@treaty/deploy`'s `deployRemote`, which uploads then calls
 * this and persists the result through the store.
 *
 * @throws if `name`, `version`, or `entry` is empty.
 */
export function recordDeployment(
	manifest: DeploymentManifest,
	name: string,
	version: string,
	entry: string,
	options: RecordDeploymentOptions = {}
): DeploymentManifest {
	if (typeof name !== 'string' || name.length === 0) {
		throw new TypeError('recordDeployment: a non-empty remote name is required')
	}
	if (typeof version !== 'string' || version.length === 0) {
		throw new TypeError(`recordDeployment: remote "${name}" needs a non-empty version`)
	}
	if (typeof entry !== 'string' || entry.length === 0) {
		throw new TypeError(`recordDeployment: remote "${name}" needs a non-empty entry url`)
	}

	const previous = manifest.remotes[name]
	const kind: RemoteKind = previous?.kind ?? options.kind ?? 'route'

	// Append the version to history (de-duplicated) and make it the last/current entry.
	const priorHistory = previous ? previous.history.filter((v) => v !== version) : []
	const history = [...priorHistory, version]

	const next: RemoteDeployment = {
		name,
		currentVersion: version,
		entry,
		history,
		kind,
	}
	return repoint(manifest, name, next)
}

/**
 * **Rollback (ledger side)**: return a NEW manifest with `name` reverted to a
 * prior `toVersion` that is already in its history — repointing the served
 * version+entry without growing history, and leaving every other remote untouched.
 * No artifact is rebuilt or re-uploaded; the prior version's immutable url is
 * recovered (from `options.entry`, else from `options.entryFor`, else by swapping
 * the version segment in the current entry url). The input is never mutated.
 *
 * Rolling back to the version already current is a no-op that returns the manifest
 * unchanged. The history is preserved in full so a rollback is itself reversible
 * (you can roll "forward" to any version still in history).
 *
 * @throws if the remote is unknown, or if `toVersion` was never deployed for it
 *   (not present in {@link RemoteDeployment.history}), or if no entry url resolves.
 */
export function rollbackTo(
	manifest: DeploymentManifest,
	name: string,
	toVersion: string,
	options: RollbackToOptions = {}
): DeploymentManifest {
	const previous = manifest.remotes[name]
	if (!previous) {
		throw new RangeError(`rollbackTo: unknown remote "${name}"`)
	}
	if (typeof toVersion !== 'string' || toVersion.length === 0) {
		throw new TypeError(`rollbackTo: remote "${name}" needs a non-empty target version`)
	}
	if (!previous.history.includes(toVersion)) {
		throw new RangeError(
			`rollbackTo: remote "${name}" was never deployed at version "${toVersion}" (history: ${previous.history.join(', ') || '<none>'})`
		)
	}
	if (toVersion === previous.currentVersion) {
		// Already serving this version: nothing to flip.
		return manifest
	}

	const entry = resolveRollbackEntry(previous, toVersion, options)
	const next: RemoteDeployment = {
		name: previous.name,
		currentVersion: toVersion,
		entry,
		// History is preserved verbatim — a rollback re-points, it does not rewrite
		// the record of what has been deployed.
		history: previous.history,
		kind: previous.kind,
	}
	return repoint(manifest, name, next)
}

/** Options for {@link rollbackTo}. */
export interface RollbackToOptions {
	/** Explicit entry url that `toVersion` is served from (skips derivation). */
	readonly entry?: string
	/** Derive the `toVersion` entry url from the remote's current deployment. */
	readonly entryFor?: (toVersion: string, current: RemoteDeployment) => string
}

/**
 * Serialize a deployment manifest to canonical JSON: remotes emitted in sorted key
 * order, history arrays preserved in deploy order, so the output is stable and
 * diffable across deploys. `space` controls indentation (default `2`; `0` for a
 * compact single line).
 */
export function serializeDeploymentManifest(manifest: DeploymentManifest, space: number = 2): string {
	const remotes: Record<string, RemoteDeployment> = {}
	for (const name of Object.keys(manifest.remotes).sort()) {
		const r = manifest.remotes[name]!
		remotes[name] = {
			name: r.name,
			currentVersion: r.currentVersion,
			entry: r.entry,
			history: [...r.history],
			kind: r.kind,
		}
	}
	const ordered = {
		schema: manifest.schema,
		...(manifest.app !== undefined ? { app: manifest.app } : {}),
		remotes,
	}
	return JSON.stringify(ordered, null, space)
}

/**
 * Parse a deployment manifest from the JSON {@link serializeDeploymentManifest}
 * produces, validating its shape. The result is frozen, so
 * `parseDeploymentManifest(serializeDeploymentManifest(m))` round-trips to a value
 * deep-equal to `m`.
 *
 * @throws if the JSON is malformed or does not match the schema.
 */
export function parseDeploymentManifest(json: string): DeploymentManifest {
	let raw: unknown
	try {
		raw = JSON.parse(json)
	} catch (err) {
		throw new SyntaxError(`parseDeploymentManifest: invalid JSON: ${(err as Error).message}`)
	}
	if (typeof raw !== 'object' || raw === null) {
		throw new TypeError('parseDeploymentManifest: manifest must be a JSON object')
	}
	const obj = raw as Record<string, unknown>
	if (obj['schema'] !== DEPLOYMENT_MANIFEST_SCHEMA) {
		throw new TypeError(
			`parseDeploymentManifest: unsupported schema (expected ${DEPLOYMENT_MANIFEST_SCHEMA})`
		)
	}
	if (typeof obj['remotes'] !== 'object' || obj['remotes'] === null) {
		throw new TypeError('parseDeploymentManifest: manifest is missing a "remotes" object')
	}

	const remotes: Record<string, RemoteDeployment> = {}
	for (const [name, value] of Object.entries(obj['remotes'] as Record<string, unknown>)) {
		remotes[name] = parseRemote(name, value)
	}

	return freezeManifest({
		schema: DEPLOYMENT_MANIFEST_SCHEMA,
		...(typeof obj['app'] === 'string' ? { app: obj['app'] } : {}),
		remotes,
	})
}

/**
 * Reference {@link DeploymentManifestStore} that keeps the manifest in memory —
 * the zero-dependency store for tests and dry-runs. Round-trips through
 * serialize/parse on every load/save so it exercises the exact persistence path a
 * real (file/object-store) implementation takes, catching shape bugs the same way.
 */
export class MemoryDeploymentStore implements DeploymentManifestStore {
	#json: string | undefined

	/** Seed the store with an initial manifest (optional). */
	constructor(initial?: DeploymentManifest) {
		this.#json = initial ? serializeDeploymentManifest(initial) : undefined
	}

	load(): DeploymentManifest | undefined {
		return this.#json === undefined ? undefined : parseDeploymentManifest(this.#json)
	}

	save(manifest: DeploymentManifest): void {
		this.#json = serializeDeploymentManifest(manifest)
	}

	/** The raw persisted JSON, or `undefined` if nothing has been saved (test aid). */
	get raw(): string | undefined {
		return this.#json
	}
}

/** Validate + normalize one remote entry parsed from JSON. */
function parseRemote(name: string, value: unknown): RemoteDeployment {
	if (typeof value !== 'object' || value === null) {
		throw new TypeError(`parseDeploymentManifest: remote "${name}" must be an object`)
	}
	const r = value as Record<string, unknown>
	const currentVersion = r['currentVersion']
	const entry = r['entry']
	const kind = r['kind']
	const history = r['history']

	if (typeof currentVersion !== 'string' || currentVersion.length === 0) {
		throw new TypeError(`parseDeploymentManifest: remote "${name}" needs a non-empty currentVersion`)
	}
	if (typeof entry !== 'string' || entry.length === 0) {
		throw new TypeError(`parseDeploymentManifest: remote "${name}" needs a non-empty entry`)
	}
	if (kind !== 'host' && kind !== 'route' && kind !== 'lib') {
		throw new TypeError(`parseDeploymentManifest: remote "${name}" kind must be host|route|lib`)
	}
	if (!Array.isArray(history) || history.some((v) => typeof v !== 'string' || v.length === 0)) {
		throw new TypeError(`parseDeploymentManifest: remote "${name}" needs a string[] history`)
	}
	if (!history.includes(currentVersion)) {
		throw new TypeError(
			`parseDeploymentManifest: remote "${name}" currentVersion "${currentVersion}" is not in its history`
		)
	}
	return {
		name,
		currentVersion,
		entry,
		history: history as string[],
		kind,
	}
}

/** Resolve the entry url a rollback should serve `toVersion` from. */
function resolveRollbackEntry(
	current: RemoteDeployment,
	toVersion: string,
	options: RollbackToOptions
): string {
	const entry =
		options.entry ??
		(options.entryFor
			? options.entryFor(toVersion, current)
			: swapVersionSegment(current.entry, current.currentVersion, toVersion))
	if (typeof entry !== 'string' || entry.length === 0) {
		throw new TypeError(
			`rollbackTo: remote "${current.name}" needs an entry url for version "${toVersion}" (pass options.entry or options.entryFor)`
		)
	}
	return entry
}

/** Swap a `/version/` segment in a stamped url; best-effort fallback otherwise. */
function swapVersionSegment(url: string, fromVersion: string, toVersion: string): string {
	const segment = `/${fromVersion}/`
	if (url.includes(segment)) return url.replace(segment, `/${toVersion}/`)
	return url.replace(fromVersion, toVersion)
}

/** Return a new manifest with exactly `name` repointed to `deployment`. */
function repoint(
	manifest: DeploymentManifest,
	name: string,
	deployment: RemoteDeployment
): DeploymentManifest {
	const remotes: Record<string, RemoteDeployment> = { ...manifest.remotes, [name]: deployment }
	return freezeManifest({
		schema: manifest.schema,
		...(manifest.app !== undefined ? { app: manifest.app } : {}),
		remotes,
	})
}

/** Deep-freeze a manifest (entries + their history arrays) so it cannot be mutated. */
function freezeManifest(manifest: DeploymentManifest): DeploymentManifest {
	for (const remote of Object.values(manifest.remotes)) {
		Object.freeze(remote.history)
		Object.freeze(remote)
	}
	Object.freeze(manifest.remotes)
	return Object.freeze(manifest)
}
