/**
 * @module
 *
 * Rspack/webpack adapter for `@treaty/module-federation`. It turns the
 * framework-agnostic {@link NormalizedMfConfig} (or raw {@link MfOptions}) into
 * the options object the `@module-federation/enhanced` `ModuleFederationPlugin`
 * accepts — the `remotes`/`exposes`/`shared` shape that bundler understands.
 *
 * Treaty is a compiler, not a host: the developer never instantiates the plugin
 * by hand. `@treaty/rspack` calls {@link toRspackModuleFederation} to build the
 * options and constructs the plugin for them automatically. The
 * `@module-federation/enhanced` types are referenced **structurally** here so
 * this package typechecks without that peer dependency installed.
 */

import {
	generateMfConfig,
	type MfOptions,
	type NormalizedMfConfig,
	type SharedConfig,
} from './config.js'

/**
 * A single shared-dependency entry in the form `@module-federation/enhanced`
 * expects: a policy object keyed by package name. Declared structurally so we
 * do not depend on the peer package's types at build time.
 */
export interface RspackSharedConfig {
	singleton?: boolean
	eager?: boolean
	requiredVersion?: string
	version?: string
	strictVersion?: boolean
}

/**
 * The options object accepted by the `@module-federation/enhanced`
 * `ModuleFederationPlugin` constructor. This is the subset Treaty generates;
 * the real plugin options are a structural superset and remain assignable.
 */
export interface RspackModuleFederationOptions {
	/** Container/library name for this app. */
	name: string
	/** This app's own remote-entry filename. */
	filename: string
	/** Consumed remotes as `alias -> "name@entryUrl"` strings. */
	remotes: Record<string, string>
	/** Exposed modules as `publicPath -> localModulePath`. */
	exposes: Record<string, string>
	/** Shared dependency policy keyed by package name. */
	shared: Record<string, RspackSharedConfig>
}

/** Render a normalized remote to the `name@entry` string `enhanced` expects. */
function toRemoteString(remote: { name: string; entry: string }): string {
	// If the entry already carries an `name@` prefix, trust it verbatim.
	return remote.entry.includes('@') && /^[\w@./-]+@\w+:\/\//.test(remote.entry)
		? remote.entry
		: `${remote.name}@${remote.entry}`
}

/** Copy a normalized {@link SharedConfig} into the plugin's shared shape. */
function toRspackShared(shared: Readonly<Record<string, SharedConfig>>): Record<string, RspackSharedConfig> {
	const out: Record<string, RspackSharedConfig> = {}
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
 * `@module-federation/enhanced` `ModuleFederationPlugin` options. The result
 * can be spread straight into `new ModuleFederationPlugin(options)`.
 */
export function toRspackModuleFederation(
	input: NormalizedMfConfig | MfOptions = {}
): RspackModuleFederationOptions {
	const config = isNormalized(input) ? input : generateMfConfig(input)

	// Federation switched off: emit inert options (no remotes/exposes/shared) so a
	// disabled config never wires the plugin's federation surface. The host
	// identity is still resolved so the shape stays uniform for callers.
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
		shared: toRspackShared(config.shared),
	}
}

/**
 * Narrow the union accepted by {@link toRspackModuleFederation}: a value is
 * already a {@link NormalizedMfConfig} when it carries the resolved `shared`
 * record produced by `generateMfConfig` (as opposed to the user-facing
 * `MfOptions` whose `shared` values may be `true`). We key off `filename`,
 * which `MfOptions` makes optional but the normalized form always sets.
 */
function isNormalized(input: NormalizedMfConfig | MfOptions): input is NormalizedMfConfig {
	return (
		typeof (input as NormalizedMfConfig).filename === 'string' &&
		typeof (input as NormalizedMfConfig).name === 'string' &&
		(input as NormalizedMfConfig).shared !== undefined &&
		(input as NormalizedMfConfig).remotes !== undefined &&
		(input as NormalizedMfConfig).exposes !== undefined
	)
}
