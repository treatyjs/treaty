/**
 * @module
 *
 * The typed builder options shared by the `@treaty/build` architect builders and
 * the structural Rspack/Module-Federation shapes they produce. Angular's
 * architect validates a target's options against the builder's `schema.json` and
 * hands them to the builder as a `json.JsonObject`; these interfaces describe the
 * exact same shape with real types so the builder bodies are type-checked.
 *
 * Treaty is a compiler, not a host: the developer points an `angular.json` target
 * at `@treaty/build:application` / `@treaty/build:dev-server` and configures
 * nothing else. Every Treaty app is a Module Federation host automatically, so
 * the options below are deliberately minimal — `entry` and `outputPath` for the
 * build, `port` for the dev server, and an optional `remotes` map for apps that
 * consume other federated apps. Everything else is derived.
 */

import type { MfOptions } from '@treaty/module-federation'

/**
 * Options for the `@treaty/build:application` builder (the `ng build` path).
 *
 * Validated against `application/schema.json`; both must stay in sync. Each field
 * is documented on the schema too so `ng build --help` surfaces it.
 */
export interface ApplicationBuilderOptions {
	/**
	 * The application entry point, relative to the workspace root
	 * (e.g. `src/main.ts`). This is the module Rspack starts the graph from and
	 * the implicit Module Federation host entry.
	 */
	readonly entry: string
	/**
	 * Directory the compiled bundle is written to, relative to the workspace root.
	 * Defaults to `dist/<project>` when omitted by the schema.
	 */
	readonly outputPath: string
	/**
	 * This app's federation name (the Module Federation container/library name).
	 * Defaults to the project name resolved from the architect target when omitted.
	 */
	readonly name?: string
	/**
	 * Remotes this app consumes, keyed by the local alias used in dynamic imports
	 * (`import('dashboard/Widget')` → key `"dashboard"`); the value is the remote
	 * entry URL. Omitted ⇒ a pure host. Because federation is automatic, an app
	 * that exposes nothing and consumes nothing still builds as a valid host.
	 */
	readonly remotes?: Readonly<Record<string, string>>
	/**
	 * Modules this app exposes to other apps, keyed by the public import path
	 * (`"./Widget"` → `"./src/app/widget.ts"`). Omitted ⇒ the app exposes nothing
	 * but remains a valid host.
	 */
	readonly exposes?: Readonly<Record<string, string>>
	/**
	 * Produce a minified production build. `ng build` sets this through the
	 * `production` configuration; the dev-server build leaves it off.
	 */
	readonly optimization?: boolean
}

/**
 * Options for the `@treaty/build:dev-server` builder (the `ng serve` path).
 *
 * Validated against `dev-server/schema.json`. The server compiles the same
 * federated host the build builder produces and serves it with HMR.
 */
export interface DevServerBuilderOptions {
	/**
	 * The application entry point, relative to the workspace root. Mirrors
	 * {@link ApplicationBuilderOptions.entry} so `ng serve` and `ng build` start
	 * the graph from the same module.
	 */
	readonly entry: string
	/**
	 * Port the dev server listens on. Defaults to `4200` (the Angular CLI default)
	 * via the schema.
	 */
	readonly port: number
	/** Host interface the dev server binds to. Defaults to `localhost`. */
	readonly host?: string
	/**
	 * This app's federation name. Defaults to the resolved project name. Kept
	 * stable across serve/build so remotes resolve the same container.
	 */
	readonly name?: string
	/**
	 * Remotes this app consumes while serving. Same shape and meaning as
	 * {@link ApplicationBuilderOptions.remotes}.
	 */
	readonly remotes?: Readonly<Record<string, string>>
	/**
	 * Modules this app exposes while serving. Same shape and meaning as
	 * {@link ApplicationBuilderOptions.exposes}.
	 */
	readonly exposes?: Readonly<Record<string, string>>
}

/**
 * Translate the build-builder's flat `remotes`/`exposes`/`name` options into the
 * {@link MfOptions} the Treaty Module Federation generator consumes. Centralised
 * so the build and serve builders derive federation identically — the principle
 * that a Treaty app federates the same way however it is run.
 */
export function toMfOptions(options: {
	readonly name?: string
	readonly remotes?: Readonly<Record<string, string>>
	readonly exposes?: Readonly<Record<string, string>>
}): MfOptions {
	const mf: {
		name?: string
		remotes?: Record<string, string>
		exposes?: Record<string, string>
	} = {}
	if (options.name !== undefined) mf.name = options.name
	if (options.remotes !== undefined) mf.remotes = { ...options.remotes }
	if (options.exposes !== undefined) mf.exposes = { ...options.exposes }
	return mf
}
