/**
 * @module
 *
 * The `@treaty/build:dev-server` architect builder — the `ng serve` path. It
 * builds the same federated Treaty Rspack config the application builder
 * produces and serves it with `@rspack/dev-server` (HMR on). The run resolves a
 * {@link BuilderOutput} once the server is listening and stays alive until
 * architect tears the run down (the server is closed in the registered teardown).
 *
 * Both `@rspack/core` and `@rspack/dev-server` are peer dependencies resolved
 * lazily from the consuming workspace and declared structurally; Treaty owns no
 * compilation or serving logic of its own — it is a compiler, not a host.
 */

import { createRequire } from 'node:module'
import { createBuilder } from '@angular-devkit/architect'
import type { BuilderContext, BuilderOutput } from '@angular-devkit/architect'
import type { json } from '@angular-devkit/core'
import { createTreatyRspackConfig, type TreatyRspackConfig } from './config.js'
import { toMfOptions, type DevServerBuilderOptions } from './options.js'

/** Structural shape of the `@rspack/core` compiler passed to the dev server. */
interface RspackCompiler {
	readonly _treatyCompiler?: never
}

/** The callable `@rspack/core` default export. */
type RspackFactory = (config: TreatyRspackConfig) => RspackCompiler

/** `@rspack/dev-server` options we set (a structural subset of its real options). */
interface RspackDevServerOptions {
	port: number
	host: string
	hot: boolean
}

/** Structural shape of an `@rspack/dev-server` instance. */
interface RspackDevServer {
	start(): Promise<void>
	stop(): Promise<void>
}

/** The `@rspack/dev-server` constructor: `new Server(options, compiler)`. */
type RspackDevServerCtor = new (
	options: RspackDevServerOptions,
	compiler: RspackCompiler
) => RspackDevServer

/** Load the `@rspack/core` factory from the consuming workspace (peer dependency). */
function loadRspack(): RspackFactory {
	const require = createRequire(import.meta.url)
	let mod: { rspack?: RspackFactory; default?: RspackFactory }
	try {
		mod = require('@rspack/core') as { rspack?: RspackFactory; default?: RspackFactory }
	} catch {
		throw new Error(
			'@treaty/build: "@rspack/core" is required to serve a Treaty app but is not installed. ' +
				'Add @rspack/core to your workspace dependencies.'
		)
	}
	const factory = mod.rspack ?? mod.default
	if (typeof factory !== 'function') {
		throw new Error('@treaty/build: "@rspack/core" did not export a callable rspack() factory.')
	}
	return factory
}

/** Load the `@rspack/dev-server` constructor from the consuming workspace (peer dependency). */
function loadRspackDevServer(): RspackDevServerCtor {
	const require = createRequire(import.meta.url)
	let mod: { RspackDevServer?: RspackDevServerCtor; default?: RspackDevServerCtor }
	try {
		mod = require('@rspack/dev-server') as {
			RspackDevServer?: RspackDevServerCtor
			default?: RspackDevServerCtor
		}
	} catch {
		throw new Error(
			'@treaty/build: "@rspack/dev-server" is required for `ng serve` but is not installed. ' +
				'Add @rspack/dev-server to your workspace dependencies.'
		)
	}
	const ctor = mod.RspackDevServer ?? mod.default
	if (typeof ctor !== 'function') {
		throw new Error('@treaty/build: "@rspack/dev-server" did not export a constructable server.')
	}
	return ctor
}

/**
 * Run the Treaty dev server for the `dev-server` target. Exposed (alongside the
 * wrapped builder) so the serve flow can be unit-tested with a mock context.
 *
 * The returned promise resolves once the server is listening; the server keeps
 * running until architect tears the run down, at which point the teardown
 * registered on the context stops it.
 */
export async function runDevServer(
	options: DevServerBuilderOptions,
	context: BuilderContext
): Promise<BuilderOutput> {
	const name = options.name ?? context.target?.project
	const host = options.host ?? 'localhost'

	const config = createTreatyRspackConfig({
		workspaceRoot: context.workspaceRoot,
		entry: options.entry,
		// The dev server always builds unminified; output path is unused while
		// serving from memory but keeps the config shape valid.
		outputPath: 'dist/.treaty-serve',
		optimization: false,
		moduleFederation: toMfOptions({
			name,
			remotes: options.remotes,
			exposes: options.exposes,
		}),
	})

	const rspack = loadRspack()
	const RspackDevServer = loadRspackDevServer()

	const compiler = rspack(config)
	const server = new RspackDevServer({ port: options.port, host, hot: true }, compiler)

	context.addTeardown(async () => {
		await server.stop()
	})

	try {
		await server.start()
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error)
		context.logger.error(message)
		return { success: false, error: message }
	}

	const url = `http://${host}:${options.port}`
	context.reportStatus(`Treaty dev server (federated host${name ? ` "${name}"` : ''}) listening on ${url}`)
	context.logger.info(`Treaty dev server listening on ${url}`)

	return { success: true, baseUrl: url, port: options.port }
}

/**
 * The architect builder for `@treaty/build:dev-server`. `createBuilder` wraps the
 * handler so architect validates options against `dev-server/schema.json`
 * (defaults applied) before our typed body runs.
 */
const builder = createBuilder<DevServerBuilderOptions & json.JsonObject>((options, context) =>
	runDevServer(options, context)
)

export default builder
