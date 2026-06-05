/**
 * @module
 *
 * Eject helpers for `@treaty/module-federation`. Treaty federation is
 * **zero-config**: every app is a host and the normalized config is generated
 * on the fly by {@link generateMfConfig}, so a developer never has to see, let
 * alone write, a federation config. This module is the escape hatch for the
 * minority of apps that want to *customize* that generated config by hand —
 * they can EJECT it to a file, edit it, and feed it back in. Ejecting is never
 * required; it is purely opt-in.
 *
 * {@link exportMfConfig} produces a plain, serializable, stable-ordered view of
 * the normalized config (the host identity plus the `remotes`/`exposes`/`shared`
 * maps), and {@link writeMfConfig} writes that view to disk as either a JSON
 * document or a ready-to-import TypeScript/JavaScript module, inferred from the
 * target path's extension.
 *
 * The `examples/everything-app/federation/*.config.ts` files are the optional
 * eject example: they show what a hand-customized, checked-in federation config
 * looks like. They are not needed to use federation — they exist only to
 * demonstrate the eject workflow.
 */

import { generateMfConfig, type MfOptions, type NormalizedMfConfig, type SharedConfig } from './config.js'

/**
 * A serializable, human-readable snapshot of a {@link NormalizedMfConfig}. Every
 * field is a plain JSON value (no functions, no `undefined`), and the maps are
 * key-sorted so the output is deterministic — diff-friendly when checked in and
 * stable across runs. This is exactly the shape a developer edits after an eject
 * and the shape {@link writeMfConfig} serializes.
 */
export interface ExportedMfConfig {
	/** Whether federation is active; mirrors {@link NormalizedMfConfig.enabled}. */
	readonly enabled: boolean
	/** The container/library name for this app. */
	readonly name: string
	/** This app's own remote-entry filename. */
	readonly filename: string
	/** Consumed remotes, key-sorted by local alias → `{ name, entry }`. */
	readonly remotes: Record<string, { readonly name: string; readonly entry: string }>
	/** Exposed modules, key-sorted by public path → local module path. */
	readonly exposes: Record<string, string>
	/** Fully-resolved shared policy, key-sorted by package name. */
	readonly shared: Record<string, SharedConfig>
}

/** Copy a record into a new object with its keys in sorted order. */
function sortedRecord<V>(record: Readonly<Record<string, V>>): Record<string, V> {
	const out: Record<string, V> = {}
	for (const key of Object.keys(record).sort()) {
		out[key] = record[key] as V
	}
	return out
}

/** Strip `undefined` fields from a {@link SharedConfig} so the export is clean. */
function compactShared(cfg: SharedConfig): SharedConfig {
	const out: { -readonly [K in keyof SharedConfig]: SharedConfig[K] } = {}
	if (cfg.singleton !== undefined) out.singleton = cfg.singleton
	if (cfg.eager !== undefined) out.eager = cfg.eager
	if (cfg.version !== undefined) out.version = cfg.version
	if (cfg.requiredVersion !== undefined) out.requiredVersion = cfg.requiredVersion
	if (cfg.strictVersion !== undefined) out.strictVersion = cfg.strictVersion
	return out
}

/**
 * Build the serializable, human-readable form of a Treaty federation config.
 *
 * Accepts the same {@link MfOptions} as {@link generateMfConfig} (so a developer
 * can eject straight from their declarative options) or an already-normalized
 * {@link NormalizedMfConfig}. The result is a plain object with key-sorted
 * `remotes`/`exposes`/`shared` and no `undefined`/function values — safe to
 * `JSON.stringify`, diff, or hand-edit. It round-trips: passing the result's
 * fields back into {@link generateMfConfig} reproduces the same normalized
 * config.
 */
export function exportMfConfig(input: MfOptions | NormalizedMfConfig = {}): ExportedMfConfig {
	const config = isNormalized(input) ? input : generateMfConfig(input)

	const shared: Record<string, SharedConfig> = {}
	for (const [pkg, cfg] of Object.entries(config.shared)) {
		shared[pkg] = compactShared(cfg)
	}

	return {
		enabled: config.enabled,
		name: config.name,
		filename: config.filename,
		remotes: sortedRecord(config.remotes),
		exposes: sortedRecord(config.exposes),
		shared: sortedRecord(shared),
	}
}

/**
 * Render an {@link ExportedMfConfig} as the text of a file at `path`. The format
 * is chosen from the extension: `.json` ⇒ a pretty-printed JSON document; `.ts`
 * / `.mts` / `.cts` / `.js` / `.mjs` / `.cjs` (or anything else) ⇒ an ES module
 * that imports `@treaty/module-federation` and re-runs {@link generateMfConfig}
 * over the ejected options, so the written file stays a *live* config a bundler
 * can import — while remaining fully editable.
 */
export function renderMfConfigFile(config: ExportedMfConfig, path: string): string {
	if (/\.json$/i.test(path)) {
		return `${JSON.stringify(config, null, '\t')}\n`
	}

	// A TS/JS module: feed the ejected, normalized options back through
	// `generateMfConfig` so the file is the editable source of truth and the
	// bundler still consumes a normalized config. `enabled`/`shared` are passed
	// explicitly so an ejected disabled or custom-shared config keeps its intent.
	const options: MfOptions = {
		enabled: config.enabled,
		name: config.name,
		filename: config.filename,
		remotes: config.remotes,
		exposes: config.exposes,
		shareAngular: false,
		shared: config.shared,
	}
	const literal = JSON.stringify(options, null, '\t')
	return (
		'/**\n' +
		' * Ejected Treaty Module Federation config.\n' +
		' *\n' +
		' * Auto-generated by `writeMfConfig` from the zero-config defaults; edit\n' +
		' * freely. `generateMfConfig` re-normalizes these options at import time, so\n' +
		' * this stays a live config the bundler can consume. Delete this file to fall\n' +
		' * back to fully automatic, zero-config federation.\n' +
		' */\n' +
		"import { generateMfConfig } from '@treaty/module-federation'\n" +
		'\n' +
		`export const mfConfig = generateMfConfig(${literal})\n` +
		'\n' +
		'export default mfConfig\n'
	)
}

/**
 * Eject the generated federation config to a file on disk so a developer can
 * customize it. `input` is the same {@link MfOptions}/{@link NormalizedMfConfig}
 * {@link exportMfConfig} accepts; the file format is inferred from `path`'s
 * extension (`.json` ⇒ JSON, otherwise a re-importable TS/JS module). Parent
 * directories are created as needed. Returns the {@link ExportedMfConfig} that
 * was written. This is opt-in: not writing the file leaves federation fully
 * zero-config.
 */
export async function writeMfConfig(
	input: MfOptions | NormalizedMfConfig,
	path: string
): Promise<ExportedMfConfig> {
	const config = exportMfConfig(input)
	const contents = renderMfConfigFile(config, path)
	const { mkdir, writeFile } = await import('node:fs/promises')
	const { dirname } = await import('node:path')
	await mkdir(dirname(path), { recursive: true })
	await writeFile(path, contents, 'utf8')
	return config
}

/** Narrow the input union: a value already carries the resolved normalized shape. */
function isNormalized(input: MfOptions | NormalizedMfConfig): input is NormalizedMfConfig {
	return (
		typeof (input as NormalizedMfConfig).enabled === 'boolean' &&
		typeof (input as NormalizedMfConfig).name === 'string' &&
		typeof (input as NormalizedMfConfig).filename === 'string' &&
		(input as NormalizedMfConfig).shared !== undefined &&
		(input as NormalizedMfConfig).remotes !== undefined &&
		(input as NormalizedMfConfig).exposes !== undefined
	)
}
