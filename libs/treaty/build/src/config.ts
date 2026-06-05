/**
 * @module
 *
 * Builds the Rspack configuration the `@treaty/build` builders run. There is no
 * bundler logic here beyond assembling a config object: the Treaty lowering is
 * done by the `@treaty/rspack` loader (which the {@link TreatyRspackPlugin}
 * wires) and federation by `@treaty/module-federation` (which the same plugin
 * adds automatically). This keeps the builders thin — they translate architect
 * options into an Rspack config and hand it to `@rspack/core`.
 *
 * `@rspack/core` is a peer dependency and is not installed in this package, so
 * its `Configuration` shape is declared structurally below; the real type is a
 * superset and remains assignable. Treaty is a compiler, not a host.
 */

import path from 'node:path'
import { TreatyRspackPlugin } from '@treaty/rspack/plugin'
import type { MfOptions } from '@treaty/module-federation'

/**
 * The slice of an `@rspack/core` `Configuration` the Treaty builders produce.
 * Declared structurally so this package typechecks without the peer installed —
 * the real `Configuration` is a structural superset of this and is assignable
 * to it, and the object produced here is assignable to the real `Configuration`.
 */
export interface TreatyRspackConfig {
	/** Build mode. The build builder sets `production`/`development` from `optimization`. */
	mode: 'production' | 'development' | 'none'
	/** Absolute project/context directory the build resolves entries against. */
	context: string
	/** The application entry module(s), keyed by chunk name. */
	entry: Record<string, string>
	/** Output configuration: where the federated host/remote bundle is written. */
	output: {
		/** Absolute output directory. */
		path: string
		/** Output filename template. */
		filename: string
		/** Public path the federation runtime loads chunks from. */
		publicPath: string
		/** Clean the output directory before emitting. */
		clean: boolean
	}
	/** Resolve config; the Treaty plugin appends its authoring extensions here. */
	resolve: { extensions: string[] }
	/** Module rules; the Treaty plugin pushes the loader rule here. */
	module: { rules: unknown[] }
	/** Plugin list; the Treaty plugin and its auto-MF plugin are added here. */
	plugins: unknown[]
	/** Devtool / source-map strategy. */
	devtool: string | false
}

/** Inputs the {@link createTreatyRspackConfig} factory needs from a builder. */
export interface TreatyRspackConfigInput {
	/** Absolute workspace/project root the build runs in. */
	readonly workspaceRoot: string
	/** Entry module path, relative to {@link TreatyRspackConfigInput.workspaceRoot}. */
	readonly entry: string
	/** Absolute or workspace-relative output directory. */
	readonly outputPath: string
	/** Whether to produce an optimized (production) build. */
	readonly optimization: boolean
	/** Module Federation options; auto-MF is always on (Treaty federates by default). */
	readonly moduleFederation: MfOptions
}

/**
 * Resolve a possibly-relative path against the workspace root, returning an
 * absolute path. Architect gives builders an un-normalized system
 * `workspaceRoot`, so we join then normalize for the current platform.
 */
function resolveFromRoot(workspaceRoot: string, target: string): string {
	return path.isAbsolute(target) ? path.normalize(target) : path.resolve(workspaceRoot, target)
}

/**
 * Assemble the Rspack {@link TreatyRspackConfig} a Treaty build/serve runs.
 *
 * The returned config carries an empty `module.rules`, `resolve.extensions`, and
 * `plugins`; the single {@link TreatyRspackPlugin} added here mutates all three
 * on `apply` — registering the Treaty loader, its authoring extensions, and the
 * auto-generated Module Federation plugin. That is the whole integration: the
 * developer points a target at the builder and gets a federated host with no
 * hand-written federation config.
 */
export function createTreatyRspackConfig(input: TreatyRspackConfigInput): TreatyRspackConfig {
	const context = path.normalize(input.workspaceRoot)
	const outputPath = resolveFromRoot(context, input.outputPath)

	return {
		mode: input.optimization ? 'production' : 'development',
		context,
		entry: { main: resolveFromRoot(context, input.entry) },
		output: {
			path: outputPath,
			filename: input.optimization ? '[name].[contenthash].js' : '[name].js',
			publicPath: 'auto',
			clean: true,
		},
		resolve: { extensions: ['.ts', '.js', '.mjs'] },
		module: { rules: [] },
		// Auto Module Federation: the Treaty plugin reads moduleFederation and adds
		// the @module-federation/enhanced plugin so every Treaty app is a host.
		plugins: [new TreatyRspackPlugin({ moduleFederation: input.moduleFederation })],
		devtool: input.optimization ? false : 'eval-source-map',
	}
}
