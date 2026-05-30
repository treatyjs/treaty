/**
 * @module
 *
 * A thin Rspack (webpack-compatible) plugin that wires the Treaty loader into a
 * build. On `apply(compiler)` it:
 *   1. registers a module rule that runs {@link treatyLoader} over Treaty's
 *      owned extensions (`.treaty`, `.tsx`, `.tjsx` by default), and
 *   2. adds those extensions to `resolve.extensions` so bare imports resolve.
 *
 * The plugin contains no compilation logic of its own — all lowering happens in
 * the loader, which delegates to the Rust authoring compiler via
 * `@treaty/compiler`. Treaty is a compiler, not a host.
 *
 * Batch vs. per-file: Rspack/webpack is a loader-based, pull pipeline — the loader
 * is invoked once per module as the graph is walked, and there is no clean hook
 * that hands a plugin the full set of owned source files up front. The core's
 * batch `transformMany` therefore has no natural wiring point here, so this
 * integration stays on the per-file loader path (which still shares one compiler
 * instance per option set for incremental-cache reuse across the build). The
 * batch path is wired in the bundlers that expose a cold-build hook (Vite's
 * `buildStart`, rsbuild/rslib's `onBeforeBuild`).
 */

import { loaderPath } from './loader.js'
import {
	DEFAULT_EXTENSIONS,
	DEFAULT_TEST,
	type TreatyLoaderOptions,
	type TreatyPluginOptions,
} from './options.js'

/** A single `use` entry on a module rule. */
interface RuleUseEntry {
	loader: string
	options?: TreatyLoaderOptions
}

/** The subset of a module rule the plugin produces. */
interface ModuleRule {
	test?: RegExp
	use?: RuleUseEntry[]
	[key: string]: unknown
}

/**
 * The slice of an Rspack/webpack `Compiler` the plugin mutates. Declared
 * structurally so the package typechecks without the peer-only `@rspack/core`
 * types installed; the real `Compiler` is assignable to this.
 */
export interface TreatyCompilerHost {
	options: {
		module?: { rules?: unknown[] }
		resolve?: { extensions?: string[] }
		[key: string]: unknown
	}
}

/**
 * Build the module rule the plugin installs. Exposed so callers who prefer to
 * wire the rule into their own config (instead of using the plugin) can reuse
 * the exact same loader configuration.
 */
export function treatyRule(options: TreatyPluginOptions = {}): ModuleRule {
	const { extensions: _extensions, test, ...compilerOptions } = options
	return {
		test: test ?? DEFAULT_TEST,
		use: [
			{
				loader: loaderPath,
				options: compilerOptions,
			},
		],
	}
}

/** Rspack/webpack plugin that registers the Treaty loader and resolves its extensions. */
export class TreatyRspackPlugin {
	/** Stable plugin name surfaced in Rspack stats/diagnostics. */
	static readonly NAME = 'TreatyRspackPlugin'

	private readonly options: TreatyPluginOptions

	constructor(options: TreatyPluginOptions = {}) {
		this.options = options
	}

	/** Mutate the compiler config: add the loader rule and resolve extensions. */
	apply(compiler: TreatyCompilerHost): void {
		const config = compiler.options

		const moduleConfig = (config.module ??= {})
		const rules = (moduleConfig.rules ??= [])
		rules.push(treatyRule(this.options))

		const resolve = (config.resolve ??= {})
		const wanted = this.options.extensions ?? DEFAULT_EXTENSIONS
		const existing = (resolve.extensions ??= [])
		for (const ext of wanted) {
			if (!existing.includes(ext)) existing.push(ext)
		}
	}
}

export default TreatyRspackPlugin
