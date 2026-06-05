/**
 * Ambient declarations for the CLI's optional bundler peers so `@treaty/cli`
 * typechecks (and the parser / dry-run paths run) without any of them installed.
 *
 * The CLI is a thin driver: it loads whichever bundler the project selected
 * (`vite` or `@rspack/core` + `@rspack/dev-server`) lazily, at the moment a
 * command actually needs to start a server or run a build. These module
 * declarations capture only the slice of each peer's surface the CLI touches;
 * the real packages' exports are assignable to these structural shapes.
 */

declare module 'vite' {
	/** A Vite plugin object (opaque here — produced by `@treaty/vite`). */
	export interface Plugin {
		readonly name: string
		[key: string]: unknown
	}

	/** The slice of Vite's `InlineConfig` the CLI sets. */
	export interface InlineConfig {
		root?: string
		base?: string
		mode?: string
		configFile?: string | false
		plugins?: Array<Plugin | Promise<Plugin> | Array<Plugin | Promise<Plugin>>>
		build?: {
			outDir?: string
			emptyOutDir?: boolean
			target?: string | string[]
			[key: string]: unknown
		}
		server?: {
			host?: string | boolean
			port?: number
			open?: boolean
			[key: string]: unknown
		}
		[key: string]: unknown
	}

	/** A running Vite dev server (only `listen`/`close`/`printUrls` are used). */
	export interface ViteDevServer {
		listen(port?: number): Promise<ViteDevServer>
		close(): Promise<void>
		printUrls?(): void
		[key: string]: unknown
	}

	export function createServer(config?: InlineConfig): Promise<ViteDevServer>
	export function build(config?: InlineConfig): Promise<unknown>
}

declare module '@rspack/core' {
	/** A minimal Rspack configuration shape (the CLI fills in entry/output/plugins). */
	export interface Configuration {
		mode?: 'development' | 'production' | 'none'
		context?: string
		entry?: unknown
		output?: { path?: string; publicPath?: string; [key: string]: unknown }
		plugins?: unknown[]
		resolve?: { extensions?: string[]; [key: string]: unknown }
		module?: { rules?: unknown[]; [key: string]: unknown }
		[key: string]: unknown
	}

	/** A running Rspack compiler (only `run`/`close` are used for one-shot builds). */
	export interface Compiler {
		run(callback: (err: Error | null, stats: unknown) => void): void
		close(callback: (err: Error | null) => void): void
		[key: string]: unknown
	}

	export function rspack(options: Configuration): Compiler
	const _default: { (options: Configuration): Compiler }
	export default _default
}

declare module '@rspack/dev-server' {
	import type { Compiler, Configuration } from '@rspack/core'

	/** The dev-server options the CLI sets (host/port/static root). */
	export interface DevServerConfiguration {
		host?: string
		port?: number
		open?: boolean
		static?: { directory?: string } | string | boolean
		[key: string]: unknown
	}

	/** The Rspack dev server (constructed with options + a compiler). */
	export class RspackDevServer {
		constructor(options: DevServerConfiguration, compiler: Compiler)
		start(): Promise<void>
		stop(): Promise<void>
	}

	export type { Configuration }
}
