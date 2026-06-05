/**
 * @module
 *
 * Vite adapter for `@treaty/module-federation`. It turns the framework-agnostic
 * {@link NormalizedMfConfig} (or raw {@link MfOptions}) into the options object
 * the `@module-federation/vite` `federation()` plugin accepts.
 *
 * The `@module-federation/vite` options shape is close to the `enhanced` one
 * but differs in the `shared` policy (it omits webpack-specific knobs) and in
 * how remotes are spelled. Treaty owns the translation so a developer writes no
 * federation config in either bundler. The `@module-federation/vite` types are
 * referenced **structurally** here so this package typechecks without that peer
 * dependency installed.
 */

import {
	generateMfConfig,
	type MfOptions,
	type NormalizedMfConfig,
	type SharedConfig,
} from './config.js'

/** A shared-dependency policy in the form `@module-federation/vite` expects. */
export interface ViteSharedConfig {
	singleton?: boolean
	eager?: boolean
	requiredVersion?: string
	version?: string
	strictVersion?: boolean
}

/**
 * The options object accepted by the `@module-federation/vite` `federation()`
 * plugin factory. This is the subset Treaty generates; the real options are a
 * structural superset and remain assignable.
 */
export interface ViteFederationOptions {
	/** Container/library name for this app. */
	name: string
	/** This app's own remote-entry filename. */
	filename: string
	/** Consumed remotes as `alias -> "name@entryUrl"` strings. */
	remotes: Record<string, string>
	/** Exposed modules as `publicPath -> localModulePath`. */
	exposes: Record<string, string>
	/** Shared dependency policy keyed by package name. */
	shared: Record<string, ViteSharedConfig>
}

/** Render a normalized remote to the `name@entry` string the plugin expects. */
function toRemoteString(remote: { name: string; entry: string }): string {
	return remote.entry.includes('@') && /^[\w@./-]+@\w+:\/\//.test(remote.entry)
		? remote.entry
		: `${remote.name}@${remote.entry}`
}

/** Copy a normalized {@link SharedConfig} into the Vite plugin's shared shape. */
function toViteShared(shared: Readonly<Record<string, SharedConfig>>): Record<string, ViteSharedConfig> {
	const out: Record<string, ViteSharedConfig> = {}
	for (const [pkg, cfg] of Object.entries(shared)) {
		out[pkg] = {
			singleton: cfg.singleton,
			eager: cfg.eager,
			requiredVersion: cfg.requiredVersion,
			version: cfg.version,
			strictVersion: cfg.strictVersion,
		}
	}
	return out
}

/**
 * Convert a normalized config (or raw {@link MfOptions}) into ready-to-use
 * `@module-federation/vite` `federation()` options. The result can be passed
 * straight to `federation(options)` in a Vite config's `plugins` array.
 */
export function toViteFederation(
	input: NormalizedMfConfig | MfOptions = {}
): ViteFederationOptions {
	const config = isNormalized(input) ? input : generateMfConfig(input)

	// Federation switched off: emit inert options so a disabled config never wires
	// the `federation()` plugin's surface. The host identity is still resolved.
	if (config.enabled === false) {
		return { name: config.name, filename: config.filename, remotes: {}, exposes: {}, shared: {} }
	}

	const remotes: Record<string, string> = {}
	for (const [alias, remote] of Object.entries(config.remotes)) {
		remotes[alias] = toRemoteString(remote)
	}

	return {
		name: config.name,
		filename: config.filename,
		remotes,
		exposes: { ...config.exposes },
		shared: toViteShared(config.shared),
	}
}

/** See {@link toRspackModuleFederation}'s twin: narrow the input union. */
function isNormalized(input: NormalizedMfConfig | MfOptions): input is NormalizedMfConfig {
	return (
		typeof (input as NormalizedMfConfig).filename === 'string' &&
		typeof (input as NormalizedMfConfig).name === 'string' &&
		(input as NormalizedMfConfig).shared !== undefined &&
		(input as NormalizedMfConfig).remotes !== undefined &&
		(input as NormalizedMfConfig).exposes !== undefined
	)
}
