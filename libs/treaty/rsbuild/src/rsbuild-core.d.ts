/**
 * @module
 *
 * Minimal ambient declaration of the slice of `@rsbuild/core` this plugin
 * consumes. `@rsbuild/core` is a *peer* dependency: the host application
 * provides it at runtime, and it is not installed in this monorepo. To keep
 * `@treaty/rsbuild` self-typechecking without taking a hard dependency on the
 * bundler, we declare only the surface we touch — `RsbuildPlugin`, the plugin
 * `api` hooks (`transform`, `modifyRsbuildConfig`), and the Rspack escape hatch
 * (`tools.rspack`). The real types from `@rsbuild/core`, when present, are
 * structurally compatible supersets of these.
 */
declare module '@rsbuild/core' {
	/** A transform hook descriptor: which module ids to match. */
	export interface TransformDescriptor {
		/** Match modules whose resolved id satisfies this test. */
		readonly test?: RegExp
		/** Match modules with one of these resource queries. */
		readonly resourceQuery?: RegExp
	}

	/** Context passed to a `api.transform` handler. */
	export interface TransformContext {
		/** Source text of the module being transformed. */
		readonly code: string
		/** Absolute resource path of the module being transformed. */
		readonly resourcePath: string
		/** Resource query string (e.g. `?raw`), if any. */
		readonly resource: string
	}

	/** Return value of a transform handler: new code, optionally with a map. */
	export interface TransformResult {
		readonly code: string
		readonly map?: string | object
	}

	/** A single Rspack module rule (the subset this plugin sets). */
	export interface RspackModuleRule {
		readonly test?: RegExp
		readonly use?: ReadonlyArray<{ readonly loader: string; readonly options?: unknown }>
		readonly type?: string
		readonly [key: string]: unknown
	}

	/** The mutable Rspack configuration object handed to `tools.rspack`. */
	export interface RspackConfig {
		resolve?: {
			extensions?: string[]
			[key: string]: unknown
		}
		module?: {
			rules?: RspackModuleRule[]
			[key: string]: unknown
		}
		[key: string]: unknown
	}

	/** The `tools.rspack` modifier entry (function form is what we use). */
	export type RspackTool = (
		config: RspackConfig,
		utils: { readonly addRules: (rules: RspackModuleRule | RspackModuleRule[]) => void }
	) => RspackConfig | void

	/** The Rsbuild user config slice this plugin mutates. */
	export interface RsbuildConfig {
		resolve?: {
			extensions?: string[]
			[key: string]: unknown
		}
		tools?: {
			rspack?: RspackTool | RspackTool[]
			[key: string]: unknown
		}
		[key: string]: unknown
	}

	/** A Rspack `sources.Source`-compatible asset payload. */
	export interface RspackSource {
		source(): string | Buffer
		size(): number
	}

	/**
	 * The Rspack compilation handed to a `processAssets` callback. We only call
	 * `emitAsset` to add the per-server-fn chunk files and the manifest, and read
	 * `assets` to detect an already-emitted name (idempotency across stages).
	 */
	export interface RspackCompilation {
		readonly assets: Readonly<Record<string, RspackSource>>
		emitAsset(name: string, source: RspackSource, info?: Record<string, unknown>): void
	}

	/** The `sources` namespace Rspack passes into a `processAssets` callback. */
	export interface RspackSourcesNamespace {
		readonly RawSource: new (value: string | Buffer) => RspackSource
	}

	/** Arguments rsbuild passes to a `processAssets` callback. */
	export interface ProcessAssetsArgs {
		readonly assets: Readonly<Record<string, RspackSource>>
		readonly compilation: RspackCompilation
		readonly sources: RspackSourcesNamespace
		readonly environment: { readonly name: string }
	}

	/** A `processAssets` registration descriptor: which pipeline stage to run in. */
	export interface ProcessAssetsDescriptor {
		readonly stage?: string
		readonly targets?: readonly string[]
		readonly environments?: readonly string[]
	}

	/** The plugin API surface exposed to `setup(api)`. */
	export interface RsbuildPluginAPI {
		/** Register a code transform for matched modules. */
		transform(
			descriptor: TransformDescriptor,
			handler: (
				context: TransformContext
			) => TransformResult | string | Promise<TransformResult | string>
		): void
		/** Mutate the resolved Rsbuild config (used for `resolve.extensions`). */
		modifyRsbuildConfig(
			modifier: (config: RsbuildConfig) => RsbuildConfig | void | Promise<RsbuildConfig | void>
		): void
		/**
		 * Cold-build hook: invoked once before a production build starts (not for
		 * the dev server). Optional in this declaration so the plugin only batch-
		 * prewarms when the host's rsbuild actually provides it.
		 */
		onBeforeBuild?(callback: () => void | Promise<void>): void
		/**
		 * Late asset-pipeline hook used to emit the per-server-fn chunk files and
		 * the server-fn manifest collected during `transform`. Optional in this
		 * declaration so the plugin only emits chunks when the host's rsbuild
		 * provides it (and the smokes can exercise the emit core directly).
		 */
		processAssets?(
			descriptor: ProcessAssetsDescriptor,
			handler: (args: ProcessAssetsArgs) => void | Promise<void>
		): void
	}

	/** An Rsbuild plugin: a named object with a `setup(api)` entry point. */
	export interface RsbuildPlugin {
		readonly name: string
		setup(api: RsbuildPluginAPI): void | Promise<void>
	}
}
