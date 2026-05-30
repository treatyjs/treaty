/**
 * @module
 *
 * The bundler engine: pure config builders shared by `treaty dev` and
 * `treaty build`. Given a {@link ResolvedConfig} these functions produce a Vite
 * `InlineConfig` or an Rspack `Configuration` with the Treaty plugin and the
 * **automatic Module Federation** wiring already in place — the developer writes
 * no `vite.config`/`rspack.config` and no `federation()` call.
 *
 * These builders are deliberately side-effect free and do not import the bundler
 * peers: they only consume `@treaty/{vite,rspack}` (always-present workspace
 * deps) to construct plugin instances. That makes them safe to call in a dry-run
 * (the CLI smoke test does exactly this) — resolving the plugin + MF config
 * never starts a server or requires `vite`/`@rspack/core` to be installed.
 */

import { treatyWithFederation, type PluginOptions } from '@treaty/vite'
import { TreatyRspackPlugin, type TreatyPluginOptions } from '@treaty/rspack'
import type { Plugin, InlineConfig } from 'vite'
import type { Configuration } from '@rspack/core'
import type { ResolvedConfig } from './config.js'

/** A handle to a running dev server, returned by the `dev` command. */
export interface RunningServer {
	/** Which bundler is serving. */
	readonly bundler: ResolvedConfig['bundler']
	/** The URL the app is served at. */
	readonly url: string
	/** Stop the server and release its resources. */
	close(): Promise<void>
}

/**
 * Build the Vite plugin set for a Treaty app: the Treaty authoring plugin plus
 * the auto-generated `@module-federation/vite` plugin (zero-config federation).
 * Returned as the `plugins` array Vite expects — federation entries may be
 * `Promise<Plugin>`, which Vite supports natively.
 */
export function buildVitePlugins(
	config: ResolvedConfig
): Array<Plugin | Promise<Plugin>> {
	const options: PluginOptions = {
		...config.compiler,
		moduleFederation: config.moduleFederation,
	}
	return treatyWithFederation(options)
}

/**
 * Build a complete Vite `InlineConfig` for the given command. `serve` configures
 * the dev server; `build` configures the production output. The Treaty plugin +
 * auto-MF are always wired. `configFile: false` keeps the standalone CLI from
 * accidentally picking up a stray `vite.config` — Treaty owns the config.
 */
export function buildViteConfig(
	config: ResolvedConfig,
	command: 'serve' | 'build'
): InlineConfig {
	const base: InlineConfig = {
		root: config.root,
		base: config.base,
		mode: command === 'serve' ? 'development' : 'production',
		configFile: false,
		plugins: buildVitePlugins(config),
	}
	if (command === 'serve') {
		base.server = { host: config.host, port: config.port }
	} else {
		base.build = { outDir: config.outDir, emptyOutDir: true }
	}
	return base
}

/**
 * Build the Treaty Rspack plugin instance for an app. This single plugin
 * registers the Treaty loader, resolves the authoring extensions, and — because
 * federation is default-on — adds the `@module-federation/enhanced`
 * `ModuleFederationPlugin` with the auto-generated config.
 */
export function buildRspackPlugin(config: ResolvedConfig): TreatyRspackPlugin {
	const options: TreatyPluginOptions = {
		...config.compiler,
		moduleFederation: config.moduleFederation,
	}
	return new TreatyRspackPlugin(options)
}

/**
 * Build a complete Rspack `Configuration` for the given mode, with the Treaty
 * plugin (loader + resolve + auto-MF) applied. The entry/output follow the
 * project conventions resolved in {@link ResolvedConfig}.
 */
export function buildRspackConfig(
	config: ResolvedConfig,
	mode: 'development' | 'production'
): Configuration {
	const rspackConfig: Configuration = {
		mode,
		context: config.root,
		entry: config.entry,
		output: { path: config.outDir, publicPath: config.base },
		plugins: [],
	}
	// The Treaty Rspack plugin's `apply` takes a compiler-like host and mutates
	// `host.options` (the config) in place — adding the loader rule, resolving
	// the authoring extensions, and pushing the auto-generated
	// ModuleFederationPlugin. We hand it a host whose `.options` is our config so
	// the result is exactly what a hand-written rspack.config would contain — but
	// here Treaty owns the config and the developer writes none.
	buildRspackPlugin(config).apply({ options: rspackConfig })
	return rspackConfig
}
