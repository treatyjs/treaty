/**
 * @module
 *
 * `treaty dev` — start the development server for a standalone Treaty project.
 *
 * The CLI is a driver, not a host: it selects the project's bundler (Vite by
 * default, or Rspack) and starts that bundler's dev server with the Treaty
 * plugin and **automatic Module Federation** wired in. The developer configures
 * nothing — every Treaty app is a federation host out of the box.
 *
 * Both bundler peers are optional and loaded lazily, so importing this module
 * (or building the CLI) never requires `vite`/`@rspack/*` to be installed; they
 * are only needed when `dev` actually runs for that bundler.
 */

import type { ResolvedConfig } from '../config.js'
import { buildViteConfig, buildRspackConfig, type RunningServer } from '../engine.js'

/**
 * Start the dev server described by `config`. Resolves once the server is
 * listening, returning a {@link RunningServer} handle the caller can `close()`.
 * Dispatches to the configured bundler; the actual plugin + federation wiring
 * lives in `engine.ts` so `dev` and `build` share one code path.
 */
export async function runDev(config: ResolvedConfig): Promise<RunningServer> {
	return config.bundler === 'rspack' ? runViteRspackDev(config) : runViteDev(config)
}

/** Start a Vite dev server with the Treaty plugin + auto-MF. */
async function runViteDev(config: ResolvedConfig): Promise<RunningServer> {
	const vite = await import('vite')
	const inlineConfig = buildViteConfig(config, 'serve')
	const server = await vite.createServer(inlineConfig)
	await server.listen(config.port)
	server.printUrls?.()
	return {
		bundler: 'vite',
		url: `http://${config.host}:${config.port}${config.base}`,
		async close() {
			await server.close()
		},
	}
}

/** Start an Rspack dev server with the Treaty loader/plugin + auto-MF. */
async function runViteRspackDev(config: ResolvedConfig): Promise<RunningServer> {
	const { rspack } = await import('@rspack/core')
	const { RspackDevServer } = await import('@rspack/dev-server')
	const rspackConfig = buildRspackConfig(config, 'development')
	const compiler = rspack(rspackConfig)
	const server = new RspackDevServer(
		{
			host: config.host,
			port: config.port,
			static: { directory: config.root },
		},
		compiler
	)
	await server.start()
	return {
		bundler: 'rspack',
		url: `http://${config.host}:${config.port}${config.base}`,
		async close() {
			await server.stop()
			await new Promise<void>((res, rej) =>
				compiler.close((err) => (err ? rej(err) : res()))
			)
		},
	}
}
