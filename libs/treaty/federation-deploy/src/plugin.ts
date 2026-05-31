/**
 * @module
 *
 * The pluggable deploy/rollback layer for Treaty's federated modules. Treaty is
 * a **compiler**, not a host: it emits each federated module's artifact plus the
 * versioned {@link FederationManifest}. *How* an artifact actually lands somewhere
 * servable — copied to a CDN bucket, pushed to object storage, uploaded via a
 * platform API — is deployment-target-specific and therefore pluggable.
 *
 * A {@link DeployPlugin} encapsulates one deployment method: `deploy(module,
 * artifact, ctx)` publishes a module's built artifact and reports the
 * {@link ModuleDeployment} (version + url) that became live; `rollback(module,
 * toVersion, ctx)` repoints a module at an already-published prior version. The
 * {@link DeployPluginRegistry} resolves a plugin by name so CI can select a
 * method per environment without the orchestration code knowing the details.
 *
 * Two reference plugins ship here: {@link NoopDeployPlugin} (records intent, used
 * for dry-runs/tests) and {@link FsDeployPlugin} (writes artifacts to a local
 * directory and maintains a manifest file on disk — the smallest real backend).
 */

import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import {
	buildManifest,
	parseManifest,
	rollbackModule,
	serializeManifest,
	setModuleVersion,
	type FederationManifest,
	type ModuleDeployment,
	type ModuleKind,
} from './manifest.js'

/**
 * Identity of a federated module being deployed: its stable `moduleId`, the
 * `version` to publish, and what `kind` of module it is. This is what
 * {@link DeployPlugin.deploy} acts on; it mirrors the manifest's notion of a
 * module so a plugin can stamp the right manifest entry.
 */
export interface DeployModule {
	/** Stable identity of the module across versions (the manifest key). */
	readonly moduleId: string
	/** The version being published. */
	readonly version: string
	/** What kind of module this is (host / route remote / lib). */
	readonly kind: ModuleKind
}

/**
 * The built output for one module that a {@link DeployPlugin} publishes. Treaty's
 * compiler produces this; the plugin is responsible for getting it to a servable
 * location and reporting the url it ends up at.
 */
export interface DeployArtifact {
	/**
	 * Absolute path to the directory holding the module's built files (the remote
	 * entry and its chunks). Optional for backends that take bytes directly.
	 */
	readonly dir?: string
	/**
	 * The remote entry filename within {@link DeployArtifact.dir}
	 * (e.g. `remoteEntry.js` / `mf-manifest.json`). Used to compute the served url.
	 */
	readonly entry?: string
	/**
	 * Raw file contents to publish, keyed by relative path. An alternative to
	 * {@link DeployArtifact.dir} for in-memory / generated artifacts (and what the
	 * reference {@link FsDeployPlugin} writes).
	 */
	readonly files?: Readonly<Record<string, string>>
}

/**
 * Ambient context handed to every {@link DeployPlugin} call: the app the deploy
 * belongs to, an optional environment label (e.g. `'staging'`/`'prod'`), an
 * optional logger, and a free-form `params` bag for plugin-specific knobs. All
 * fields are optional so a plugin can be driven with nothing but defaults.
 */
export interface DeployContext {
	/** Informational app name (mirrors {@link FederationManifest.app}). */
	readonly app?: string
	/** Target environment label, for plugins that key behavior off it. */
	readonly env?: string
	/** Sink for human-readable progress; defaults to a no-op when omitted. */
	readonly logger?: (message: string) => void
	/** Plugin-specific parameters (bucket name, base url, credentials handle, ...). */
	readonly params?: Readonly<Record<string, unknown>>
}

/**
 * A pluggable deployment method for federated modules. One plugin = one way to
 * publish/revert an artifact. Implementations must be able to publish a new
 * version ({@link DeployPlugin.deploy}) and repoint to an already-published prior
 * version ({@link DeployPlugin.rollback}); both report the resulting
 * {@link ModuleDeployment} so the orchestrator can stamp the manifest.
 */
export interface DeployPlugin {
	/** Unique name this plugin registers under (the {@link DeployPluginRegistry} key). */
	readonly name: string
	/**
	 * Publish `module`'s `artifact` and return the deployment (version + served
	 * url) that is now live. Must not mutate its inputs.
	 */
	deploy(
		module: DeployModule,
		artifact: DeployArtifact,
		ctx: DeployContext
	): Promise<ModuleDeployment>
	/**
	 * Repoint `module` to an already-published `toVersion` and return that
	 * deployment. The artifact for `toVersion` is assumed to already exist (a
	 * rollback never rebuilds); a plugin that cannot find it should reject.
	 */
	rollback(
		module: DeployModule,
		toVersion: string,
		ctx: DeployContext
	): Promise<ModuleDeployment>
}

/**
 * A registry of {@link DeployPlugin}s keyed by name, so CI can pick a deployment
 * method by string (per app/environment) without statically importing it. Names
 * are unique; registering a duplicate name throws unless `override` is set.
 */
export class DeployPluginRegistry {
	readonly #plugins = new Map<string, DeployPlugin>()

	/**
	 * Construct a registry, optionally seeded with an initial set of plugins
	 * (registered in order, so a later duplicate name throws).
	 */
	constructor(initial: Iterable<DeployPlugin> = []) {
		for (const plugin of initial) this.register(plugin)
	}

	/**
	 * Register a plugin under its {@link DeployPlugin.name}.
	 *
	 * @param plugin The plugin to register.
	 * @param override Replace an existing plugin of the same name instead of throwing.
	 * @returns `this`, for chaining.
	 * @throws if a plugin with the same name is already registered and `override`
	 *   is not set, or if the plugin has an empty name.
	 */
	register(plugin: DeployPlugin, override = false): this {
		if (!plugin || typeof plugin.name !== 'string' || plugin.name.length === 0) {
			throw new TypeError('DeployPluginRegistry.register: plugin needs a non-empty name')
		}
		if (!override && this.#plugins.has(plugin.name)) {
			throw new RangeError(`DeployPluginRegistry: a plugin named "${plugin.name}" is already registered`)
		}
		this.#plugins.set(plugin.name, plugin)
		return this
	}

	/** Whether a plugin is registered under `name`. */
	has(name: string): boolean {
		return this.#plugins.has(name)
	}

	/**
	 * Resolve a plugin by name.
	 *
	 * @throws if no plugin is registered under `name`. Use {@link DeployPluginRegistry.tryGet}
	 *   for a non-throwing lookup.
	 */
	get(name: string): DeployPlugin {
		const plugin = this.#plugins.get(name)
		if (!plugin) {
			const known = this.list().join(', ') || '<none>'
			throw new RangeError(`DeployPluginRegistry: no plugin named "${name}" (registered: ${known})`)
		}
		return plugin
	}

	/** Resolve a plugin by name, or `undefined` if none is registered under it. */
	tryGet(name: string): DeployPlugin | undefined {
		return this.#plugins.get(name)
	}

	/** The registered plugin names, sorted ascending. */
	list(): string[] {
		return [...this.#plugins.keys()].sort()
	}
}

/**
 * Reference {@link DeployPlugin} that performs no I/O: it records the deployment
 * it *would* make and returns it. Useful for dry-runs, plan previews, and tests.
 * The served url is derived from the module id + version (override via
 * `ctx.params.baseUrl`).
 */
export class NoopDeployPlugin implements DeployPlugin {
	readonly name: string

	constructor(name = 'noop') {
		this.name = name
	}

	deploy(
		module: DeployModule,
		_artifact: DeployArtifact,
		ctx: DeployContext
	): Promise<ModuleDeployment> {
		const deployment = toDeployment(module.moduleId, module.version, ctx)
		ctx.logger?.(`[${this.name}] would deploy ${module.moduleId}@${module.version} -> ${deployment.url}`)
		return Promise.resolve(deployment)
	}

	rollback(
		module: DeployModule,
		toVersion: string,
		ctx: DeployContext
	): Promise<ModuleDeployment> {
		const deployment = toDeployment(module.moduleId, toVersion, ctx)
		ctx.logger?.(`[${this.name}] would rollback ${module.moduleId} -> ${toVersion} (${deployment.url})`)
		return Promise.resolve(deployment)
	}
}

/** Options for {@link FsDeployPlugin}. */
export interface FsDeployPluginOptions {
	/**
	 * Root directory the plugin publishes into. Each version lands under
	 * `<root>/<moduleId>/<version>/`. Defaults to the current working directory.
	 */
	readonly root?: string
	/**
	 * Path to the on-disk manifest file the plugin maintains. Defaults to
	 * `<root>/mf-manifest.json`. Created on first deploy if absent.
	 */
	readonly manifestPath?: string
	/**
	 * Base url that the published `root` is served from, used to build each
	 * deployment's url as `<baseUrl>/<moduleId>/<version>/<entry>`. Defaults to a
	 * `file://` url under `root` so the plugin is self-contained without config.
	 */
	readonly baseUrl?: string
	/** Default remote entry filename when an artifact omits one. Defaults to `remoteEntry.js`. */
	readonly defaultEntry?: string
	/** Registered name. Defaults to `'fs'`. */
	readonly name?: string
}

const DEFAULT_ENTRY = 'remoteEntry.js'
const DEFAULT_MANIFEST_FILE = 'mf-manifest.json'

/**
 * Reference {@link DeployPlugin} backed by the local filesystem — the smallest
 * real deployment backend, and the example a platform-specific plugin (S3, GCS,
 * a CDN API) is modeled on.
 *
 * On {@link FsDeployPlugin.deploy} it writes the artifact's files under
 * `<root>/<moduleId>/<version>/`, then reads-modifies-writes the on-disk
 * {@link FederationManifest} so that module points at the new version+url. On
 * {@link FsDeployPlugin.rollback} it verifies the target version was previously
 * published (its directory exists) and repoints the manifest entry without
 * rewriting any files. The manifest file is the durable record of what is live.
 */
export class FsDeployPlugin implements DeployPlugin {
	readonly name: string
	readonly #root: string
	readonly #manifestPath: string
	readonly #baseUrl: string
	readonly #defaultEntry: string

	constructor(options: FsDeployPluginOptions = {}) {
		this.name = options.name ?? 'fs'
		this.#root = options.root ?? process.cwd()
		this.#manifestPath = options.manifestPath ?? join(this.#root, DEFAULT_MANIFEST_FILE)
		this.#baseUrl = options.baseUrl ?? pathToFileUrl(this.#root)
		this.#defaultEntry = options.defaultEntry ?? DEFAULT_ENTRY
	}

	/** Absolute directory a given module version is (or would be) published to. */
	versionDir(moduleId: string, version: string): string {
		return join(this.#root, moduleId, version)
	}

	async deploy(
		module: DeployModule,
		artifact: DeployArtifact,
		ctx: DeployContext
	): Promise<ModuleDeployment> {
		const entry = artifact.entry ?? this.#defaultEntry
		const dir = this.versionDir(module.moduleId, module.version)

		const files = artifact.files ?? { [entry]: defaultEntryContents(module) }
		for (const [rel, contents] of Object.entries(files)) {
			const dest = join(dir, rel)
			await mkdir(dirname(dest), { recursive: true })
			await writeFile(dest, contents)
		}

		const url = this.#urlFor(module.moduleId, module.version, entry)
		await this.#updateManifest(ctx, (manifest) =>
			manifest.modules[module.moduleId]
				? setModuleVersion(manifest, module.moduleId, module.version, { url })
				: appendModule(manifest, module, url)
		)
		ctx.logger?.(`[${this.name}] deployed ${module.moduleId}@${module.version} -> ${url}`)
		return { version: module.version, url }
	}

	async rollback(
		module: DeployModule,
		toVersion: string,
		ctx: DeployContext
	): Promise<ModuleDeployment> {
		const dir = this.versionDir(module.moduleId, toVersion)
		const entry = await this.#findEntry(dir)
		if (!entry) {
			throw new RangeError(
				`[${this.name}] cannot rollback ${module.moduleId} to ${toVersion}: no published artifact at ${dir}`
			)
		}
		const url = this.#urlFor(module.moduleId, toVersion, entry)
		await this.#updateManifest(ctx, (manifest) => {
			if (!manifest.modules[module.moduleId]) {
				throw new RangeError(
					`[${this.name}] cannot rollback unknown module "${module.moduleId}" (not in manifest)`
				)
			}
			return rollbackModule(manifest, module.moduleId, toVersion, { url })
		})
		ctx.logger?.(`[${this.name}] rolled back ${module.moduleId} -> ${toVersion} (${url})`)
		return { version: toVersion, url }
	}

	/** Read the current on-disk manifest, or `undefined` if it has not been written yet. */
	async readManifest(): Promise<FederationManifest | undefined> {
		let json: string
		try {
			json = await readFile(this.#manifestPath, 'utf8')
		} catch (err) {
			if ((err as NodeJS.ErrnoException).code === 'ENOENT') return undefined
			throw err
		}
		return parseManifest(json)
	}

	#urlFor(moduleId: string, version: string, entry: string): string {
		return `${trimTrailingSlash(this.#baseUrl)}/${moduleId}/${version}/${entry}`
	}

	/** Read-modify-write the manifest file under a fresh read each time (CI-safe). */
	async #updateManifest(
		ctx: DeployContext,
		mutate: (manifest: FederationManifest) => FederationManifest
	): Promise<void> {
		const current = (await this.readManifest()) ?? buildManifest([], ctx.app ? { app: ctx.app } : {})
		const next = mutate(current)
		await mkdir(dirname(this.#manifestPath), { recursive: true })
		await writeFile(this.#manifestPath, serializeManifest(next))
	}

	/** Find the remote entry filename in a published version dir, preferring the default. */
	async #findEntry(dir: string): Promise<string | undefined> {
		const { readdir } = await import('node:fs/promises')
		let names: string[]
		try {
			names = await readdir(dir)
		} catch (err) {
			if ((err as NodeJS.ErrnoException).code === 'ENOENT') return undefined
			throw err
		}
		if (names.length === 0) return undefined
		return names.includes(this.#defaultEntry) ? this.#defaultEntry : names.sort()[0]
	}
}

/** Build a {@link ModuleDeployment} for a module/version using `ctx.params.baseUrl`. */
function toDeployment(moduleId: string, version: string, ctx: DeployContext): ModuleDeployment {
	const base = typeof ctx.params?.['baseUrl'] === 'string' ? (ctx.params['baseUrl'] as string) : 'noop://deploy'
	return { version, url: `${trimTrailingSlash(base)}/${moduleId}/${version}/${DEFAULT_ENTRY}` }
}

/** Add a brand-new module to a manifest (used when deploying a module the manifest has never seen). */
function appendModule(manifest: FederationManifest, module: DeployModule, url: string): FederationManifest {
	const inputs = [
		...Object.entries(manifest.modules).map(([moduleId, dep]) => ({
			moduleId,
			version: dep.version,
			url: dep.url,
			kind: manifest.kinds?.[moduleId] ?? ('route' as ModuleKind),
		})),
		{ moduleId: module.moduleId, version: module.version, url, kind: module.kind },
	]
	return buildManifest(inputs, manifest.app !== undefined ? { app: manifest.app } : {})
}

/** Placeholder remote-entry contents written when an artifact provides no files. */
function defaultEntryContents(module: DeployModule): string {
	return `// treaty federated module ${module.moduleId}@${module.version} (${module.kind})\n`
}

function trimTrailingSlash(s: string): string {
	return s.endsWith('/') ? s.slice(0, -1) : s
}

/** Convert an absolute filesystem path to a `file://` url (forward slashes). */
function pathToFileUrl(p: string): string {
	const normalized = p.replace(/\\/g, '/')
	return normalized.startsWith('/') ? `file://${normalized}` : `file:///${normalized}`
}
