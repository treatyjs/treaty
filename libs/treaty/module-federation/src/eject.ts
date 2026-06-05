/**
 * @module
 *
 * **Eject to a standalone, Treaty-free Module Federation config.**
 *
 * Treaty makes every app auto-MF: each lazy feature route and each workspace lib
 * is a versioned remote, the host shares the Angular runtime as singletons, and
 * the developer writes no `ModuleFederationPlugin` (see {@link generateMfConfig}).
 * The companion {@link exportMfConfig}/{@link writeMfConfig} eject keeps Treaty in
 * the loop — its emitted file re-imports `@treaty/module-federation`.
 *
 * This module is the *other* kind of eject: it serializes the auto-derived
 * federation config into a **plain `@module-federation/enhanced`-compatible
 * config object** — `name`, `filename`, `remotes` as `name@entry` strings,
 * `exposes`, and `shared` — that the user fully **owns**. The emitted file has no
 * `@treaty/*` import and re-runs nothing: a developer can lift it into a vanilla
 * Rspack/webpack `new ModuleFederationPlugin(config)` and take over Module
 * Federation without Treaty in the build at all.
 *
 *   - {@link exportFederationConfig} — from an {@link AppGraph} (the app's name,
 *     its lazy routes + libs, the remotes it consumes, its shared deps) build the
 *     standalone {@link StandaloneFederationConfig}.
 *   - {@link renderFederationConfigFile} — render that config as the text of a
 *     `.json` document or a self-contained `.ts`/`.js` module (a bare `export`).
 *   - {@link writeFederationConfig} — write it to a path the user owns.
 *
 * The result is the exact shape {@link toRspackModuleFederation} produces, so the
 * Rspack adapter and the ejected file agree by construction.
 */

import {
	generateMfConfig,
	type MfOptions,
	type RemoteEntry,
	type SharedConfig,
} from './config.js'
import type { LibEntry, RouteLike } from './routes.js'
import {
	toRspackModuleFederation,
	type RspackModuleFederationOptions,
	type RspackSharedConfig,
} from './rspack.js'

/**
 * A minimal description of a Treaty app's federation-relevant graph — the inputs
 * from which the auto-MF config is derived. It is a strict subset of
 * {@link MfOptions}: the app's federation `name`/`filename`, the **lazy routes**
 * and **libs** that become versioned remote *exposes*, the **remotes** the app
 * consumes, and the **shared** deps. Passing `{}` still ejects a valid host that
 * shares the Angular singletons — the zero-config default, made standalone.
 *
 * This is what a caller assembles from a compiled app (its route graph + lib
 * list) and hands to {@link exportFederationConfig} to eject.
 */
export interface AppGraph {
	/** The app's federation/container name. Defaults to the host default. */
	readonly name?: string
	/** The app's own remote-entry filename. Defaults to the host default. */
	readonly filename?: string
	/**
	 * The app's Angular routes. Every **lazy** route (`loadComponent`/
	 * `loadChildren`) is derived into one exposed, independently versioned remote.
	 */
	readonly routes?: readonly RouteLike[]
	/** Workspace libraries; each becomes one exposed, independently versioned remote. */
	readonly libs?: readonly LibEntry[]
	/** Remotes this app consumes, keyed by local import alias → entry (or `{ name, entry }`). */
	readonly remotes?: Readonly<Record<string, RemoteEntry>>
	/** Extra shared deps merged over the Angular singleton defaults. */
	readonly shared?: Readonly<Record<string, SharedConfig | true>>
	/** Manual exposes that win over the routes/libs-derived ones. */
	readonly exposes?: Readonly<Record<string, string>>
	/** Replace (don't extend) the Angular shared defaults when `false`. Defaults `true`. */
	readonly shareAngular?: boolean
	/** Semver range applied to the auto-shared Angular packages. */
	readonly angularVersion?: string
	/**
	 * Master switch. `false` ejects an inert config (no remotes/exposes/shared) so
	 * an ejected, federation-off app keeps its identity but wires nothing.
	 */
	readonly enabled?: boolean
}

/**
 * A standalone `@module-federation/enhanced`-compatible federation config: the
 * exact options object a vanilla `new ModuleFederationPlugin(config)` accepts.
 * Identical in shape to {@link RspackModuleFederationOptions} — remotes are
 * `"name@entryUrl"` strings, shared is keyed by package — and carries no Treaty
 * types, so the user owns it outright.
 */
export type StandaloneFederationConfig = RspackModuleFederationOptions

/** The shared-policy entry shape inside a {@link StandaloneFederationConfig}. */
export type StandaloneSharedConfig = RspackSharedConfig

/** Map an {@link AppGraph} to the {@link MfOptions} the generator/adapter consume. */
function graphToOptions(graph: AppGraph): MfOptions {
	const options: { -readonly [K in keyof MfOptions]: MfOptions[K] } = {}
	if (graph.enabled !== undefined) options.enabled = graph.enabled
	if (graph.name !== undefined) options.name = graph.name
	if (graph.filename !== undefined) options.filename = graph.filename
	if (graph.routes !== undefined) options.routes = graph.routes
	if (graph.libs !== undefined) options.libs = graph.libs
	if (graph.remotes !== undefined) options.remotes = graph.remotes
	if (graph.shared !== undefined) options.shared = graph.shared
	if (graph.exposes !== undefined) options.exposes = graph.exposes
	if (graph.shareAngular !== undefined) options.shareAngular = graph.shareAngular
	if (graph.angularVersion !== undefined) options.angularVersion = graph.angularVersion
	return options
}

/** Copy a record into a new object with its keys in sorted order (diff-stable). */
function sortedRecord<V>(record: Readonly<Record<string, V>>): Record<string, V> {
	const out: Record<string, V> = {}
	for (const key of Object.keys(record).sort()) {
		out[key] = record[key] as V
	}
	return out
}

/** Strip `undefined` fields from a standalone shared entry so the export is clean. */
function compactShared(cfg: StandaloneSharedConfig): StandaloneSharedConfig {
	const out: StandaloneSharedConfig = {}
	if (cfg.singleton !== undefined) out.singleton = cfg.singleton
	if (cfg.eager !== undefined) out.eager = cfg.eager
	if (cfg.version !== undefined) out.version = cfg.version
	if (cfg.requiredVersion !== undefined) out.requiredVersion = cfg.requiredVersion
	if (cfg.strictVersion !== undefined) out.strictVersion = cfg.strictVersion
	return out
}

/**
 * Eject an {@link AppGraph} to a standalone `@module-federation/enhanced`-
 * compatible config object.
 *
 * Runs the same auto-derivation Treaty uses at build time — every lazy route and
 * every lib becomes a versioned remote *expose*, the Angular runtime is shared as
 * eager singletons, the consumed remotes are normalized to `name@entry` strings —
 * then returns the result as a plain options object with **key-sorted**
 * `remotes`/`exposes`/`shared` and no `undefined` values, so it is deterministic,
 * diff-friendly, and ready to drop into a hand-written `ModuleFederationPlugin`.
 *
 * When `graph.enabled === false` the ejected config is inert (empty
 * remotes/exposes/shared) but keeps the resolved `name`/`filename`.
 *
 * @example
 * exportFederationConfig({
 *   name: 'shell',
 *   routes: [{ path: 'dashboard', loadComponent: () => import('./dashboard') }],
 *   libs: ['./libs/data-access'],
 *   remotes: { reports: 'http://localhost:4202/remoteEntry.js' },
 * })
 * // => { name: 'shell', filename: 'remoteEntry.js',
 * //      remotes: { reports: 'reports@http://localhost:4202/remoteEntry.js' },
 * //      exposes: { './libs/data-access': './libs/data-access',
 * //                 './routes/dashboard': './src/app/dashboard' },
 * //      shared: { '@angular/core': { singleton: true, eager: true, ... }, ... } }
 */
export function exportFederationConfig(graph: AppGraph = {}): StandaloneFederationConfig {
	const options = graphToOptions(graph)
	// Go through generateMfConfig first so a disabled graph yields the inert config,
	// then through the Rspack adapter so the output is the real enhanced shape.
	const normalized = generateMfConfig(options)
	const plugin = toRspackModuleFederation(normalized)

	const shared: Record<string, StandaloneSharedConfig> = {}
	for (const [pkg, cfg] of Object.entries(plugin.shared)) {
		shared[pkg] = compactShared(cfg)
	}

	return {
		name: plugin.name,
		filename: plugin.filename,
		remotes: sortedRecord(plugin.remotes),
		exposes: sortedRecord(plugin.exposes),
		shared: sortedRecord(shared),
	}
}

/**
 * Render a {@link StandaloneFederationConfig} as the text of the file at `path`.
 * The format is chosen from the extension:
 *   - `.json` ⇒ a pretty-printed JSON document.
 *   - anything else (`.ts`/`.mts`/`.cts`/`.js`/`.mjs`/`.cjs`) ⇒ a **self-contained**
 *     ES module that `export`s the config object literal with **no imports**.
 *
 * Unlike {@link renderMfConfigFile}, the emitted module never references
 * `@treaty/module-federation` — it is a frozen, owned config a vanilla bundler
 * config can import and pass straight to `new ModuleFederationPlugin(config)`.
 */
export function renderFederationConfigFile(
	config: StandaloneFederationConfig,
	path: string
): string {
	if (/\.json$/i.test(path)) {
		return `${JSON.stringify(config, null, '\t')}\n`
	}

	const literal = JSON.stringify(config, null, '\t')
	return (
		'/**\n' +
		' * Standalone Module Federation config, ejected from Treaty.\n' +
		' *\n' +
		' * This is a plain `@module-federation/enhanced` `ModuleFederationPlugin`\n' +
		' * options object — you OWN it. Nothing is imported and nothing is re-derived,\n' +
		' * so you can keep this file and drop Treaty from your build: pass\n' +
		' * `federationConfig` straight to `new ModuleFederationPlugin`.\n' +
		' */\n' +
		`export const federationConfig = ${literal}\n` +
		'\n' +
		'export default federationConfig\n'
	)
}

/**
 * Eject the auto-derived federation config of an {@link AppGraph} to a file the
 * user owns. The file format is inferred from `path`'s extension (`.json` ⇒ JSON,
 * otherwise a self-contained, import-free TS/JS module — see
 * {@link renderFederationConfigFile}). Parent directories are created as needed.
 * Returns the {@link StandaloneFederationConfig} that was written.
 *
 * This is the "take over without Treaty" escape hatch: the written file is a
 * complete, standalone `@module-federation/enhanced` config with no dependency on
 * Treaty.
 */
export async function writeFederationConfig(
	graph: AppGraph,
	path: string
): Promise<StandaloneFederationConfig> {
	const config = exportFederationConfig(graph)
	const contents = renderFederationConfigFile(config, path)
	const { mkdir, writeFile } = await import('node:fs/promises')
	const { dirname } = await import('node:path')
	await mkdir(dirname(path), { recursive: true })
	await writeFile(path, contents, 'utf8')
	return config
}
