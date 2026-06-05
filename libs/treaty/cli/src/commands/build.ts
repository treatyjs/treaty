/**
 * @module
 *
 * `treaty build` — produce a deployable production build of a standalone Treaty
 * project. Like `dev`, this drives the project's bundler (Vite or Rspack) with
 * the Treaty plugin and automatic Module Federation; the output is a static,
 * federation-ready bundle (the app's own `remoteEntry` is always emitted, so any
 * Treaty app can be consumed as a remote with zero extra config).
 *
 * The bundler peers are loaded lazily — building this module / the CLI never
 * requires `vite`/`@rspack/core` to be installed.
 */

import type { ResolvedConfig } from '../config.js'
import { buildViteConfig, buildRspackConfig } from '../engine.js'

/** The result of a completed production build. */
export interface BuildResult {
	/** Which bundler produced the build. */
	readonly bundler: ResolvedConfig['bundler']
	/** Absolute path to the emitted output directory. */
	readonly outDir: string
}

/**
 * Run a one-shot production build for `config` and resolve once it has been
 * written to {@link ResolvedConfig.outDir}. Dispatches to the configured bundler.
 */
export async function runBuild(config: ResolvedConfig): Promise<BuildResult> {
	return config.bundler === 'rspack' ? runRspackBuild(config) : runViteBuild(config)
}

/** Produce a Vite production build with the Treaty plugin + auto-MF. */
async function runViteBuild(config: ResolvedConfig): Promise<BuildResult> {
	const vite = await import('vite')
	await vite.build(buildViteConfig(config, 'build'))
	return { bundler: 'vite', outDir: config.outDir }
}

/** Produce an Rspack production build with the Treaty plugin + auto-MF. */
async function runRspackBuild(config: ResolvedConfig): Promise<BuildResult> {
	const { rspack } = await import('@rspack/core')
	const compiler = rspack(buildRspackConfig(config, 'production'))
	await new Promise<void>((resolveRun, rejectRun) => {
		compiler.run((err, stats) => {
			if (err) {
				rejectRun(err)
				return
			}
			// Surface compilation errors (which arrive on `stats`, not `err`) by
			// rejecting; a deployable build must not silently ship a broken bundle.
			const s = stats as { hasErrors?(): boolean; toString?(opts: unknown): string } | null
			if (s?.hasErrors?.()) {
				rejectRun(new Error(s.toString?.({ colors: false }) ?? 'Rspack build failed'))
				return
			}
			compiler.close((closeErr) => (closeErr ? rejectRun(closeErr) : resolveRun()))
		})
	})
	return { bundler: 'rspack', outDir: config.outDir }
}
