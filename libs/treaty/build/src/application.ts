/**
 * @module
 *
 * The `@treaty/build:application` architect builder — the `ng build` path. It
 * translates the validated {@link ApplicationBuilderOptions} into a Treaty Rspack
 * config (which auto-wires Module Federation), runs `@rspack/core` once, and
 * resolves a {@link BuilderOutput} with the emitted stats.
 *
 * `@rspack/core` is an (uninstalled-here) peer dependency, so it is required
 * lazily through `createRequire` and its compiler/stats surface is declared
 * structurally. The builder owns no compilation logic: Treaty is a compiler, and
 * all lowering happens in the `@treaty/rspack` loader the config wires.
 */

import { createRequire } from 'node:module'
import { createBuilder } from '@angular-devkit/architect'
import type { BuilderContext, BuilderOutput } from '@angular-devkit/architect'
import type { json } from '@angular-devkit/core'
import { createTreatyRspackConfig, type TreatyRspackConfig } from './config.js'
import { toMfOptions, type ApplicationBuilderOptions } from './options.js'

/** Structural shape of the `@rspack/core` stats object we read after a build. */
interface RspackStats {
	hasErrors(): boolean
	toString(options?: unknown): string
	toJson(options?: unknown): { errors?: Array<{ message?: string }> }
}

/** Structural shape of the `@rspack/core` compiler returned by `rspack(config)`. */
interface RspackCompiler {
	run(callback: (error: Error | null, stats?: RspackStats) => void): void
	close(callback: (error: Error | null) => void): void
}

/** The callable `@rspack/core` default export: `rspack(config) -> Compiler`. */
type RspackFactory = (config: TreatyRspackConfig) => RspackCompiler

/**
 * Load the `@rspack/core` factory from the consuming workspace. It is a peer
 * dependency (not bundled with `@treaty/build`), so it is resolved at runtime
 * from the workspace that runs the builder. Throws a clear, actionable error
 * when the peer is missing rather than a bare module-not-found.
 */
function loadRspack(): RspackFactory {
	const require = createRequire(import.meta.url)
	let mod: { rspack?: RspackFactory; default?: RspackFactory }
	try {
		mod = require('@rspack/core') as { rspack?: RspackFactory; default?: RspackFactory }
	} catch {
		throw new Error(
			'@treaty/build: "@rspack/core" is required to build a Treaty app but is not installed. ' +
				'Add @rspack/core to your workspace dependencies.'
		)
	}
	const factory = mod.rspack ?? mod.default
	if (typeof factory !== 'function') {
		throw new Error('@treaty/build: "@rspack/core" did not export a callable rspack() factory.')
	}
	return factory
}

/**
 * Resolve the federation/library name for this app: the explicit option wins,
 * otherwise the architect target's project name, otherwise undefined (the
 * Treaty MF generator then falls back to its own default host name).
 */
function resolveName(options: ApplicationBuilderOptions, context: BuilderContext): string | undefined {
	return options.name ?? context.target?.project
}

/**
 * Run a single Treaty Rspack build for the `application` target. Exposed
 * (alongside the wrapped builder) so it can be unit-tested with a mock context
 * without going through architect.
 */
export async function runApplicationBuild(
	options: ApplicationBuilderOptions,
	context: BuilderContext
): Promise<BuilderOutput> {
	const name = resolveName(options, context)
	const config = createTreatyRspackConfig({
		workspaceRoot: context.workspaceRoot,
		entry: options.entry,
		outputPath: options.outputPath,
		optimization: options.optimization ?? false,
		moduleFederation: toMfOptions({
			name,
			remotes: options.remotes,
			exposes: options.exposes,
		}),
	})

	context.reportStatus(`Building Treaty app${name ? ` "${name}"` : ''} with Rspack + auto Module Federation`)

	// Loading the peer and constructing the compiler can throw (peer missing or
	// the config is rejected). Surface that as a clean failing BuilderOutput so
	// `ng build` reports a handled failure rather than crashing the architect run.
	let compiler: RspackCompiler
	try {
		const rspack = loadRspack()
		compiler = rspack(config)
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error)
		context.logger.error(message)
		return { success: false, error: message }
	}

	return await new Promise<BuilderOutput>((resolve) => {
		compiler.run((runError, stats) => {
			const finish = (output: BuilderOutput): void => {
				compiler.close(() => resolve(output))
			}

			if (runError) {
				context.logger.error(runError.stack ?? runError.message)
				finish({ success: false, error: runError.message })
				return
			}
			if (!stats) {
				finish({ success: false, error: '@treaty/build: Rspack produced no stats.' })
				return
			}

			context.logger.info(stats.toString({ colors: true }))

			if (stats.hasErrors()) {
				const { errors } = stats.toJson({ errors: true })
				const message = (errors ?? [])
					.map((entry) => entry.message ?? String(entry))
					.join('\n')
				finish({ success: false, error: message || 'Treaty build failed.' })
				return
			}

			finish({ success: true, outputPath: config.output.path })
		})
	})
}

/**
 * The architect builder for `@treaty/build:application`. `createBuilder` wraps
 * the handler so architect validates options against `application/schema.json`
 * (defaults applied) before our typed body runs.
 */
const builder = createBuilder<ApplicationBuilderOptions & json.JsonObject>((options, context) =>
	runApplicationBuild(options, context)
)

export default builder
