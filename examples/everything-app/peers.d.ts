/// <reference path="../../libs/treaty/jsx/dist/ambient.d.ts" />

/**
 * Ambient declarations for the OPTIONAL bundler / federation peer packages this
 * example references but does not install.
 *
 * Treaty's plugins reference these peers *structurally* (each ships its own
 * sibling `*-peer.d.ts` shim) so they typecheck without the bundler installed.
 * When this example consumes the plugins' built `.d.ts`, those sibling shims are
 * not in scope, so we restate the same minimal structural shapes here. The real
 * packages' exports are assignable supersets of these — the developer's build
 * provides the genuine implementations at run time.
 *
 * Treaty is a compiler, not a host: these declarations exist only to keep the
 * authoring-time configs type-clean; nothing here runs.
 *
 * The authoring-format module shims (`*.treaty` / `*.tjsx`) are NOT declared
 * here: they ship from `@treaty/jsx` (its `./ambient` entry) and are picked up
 * by the single triple-slash reference above, so no per-app `declare module`
 * block is needed. This workspace consumes `@treaty/jsx` from its built `dist`
 * rather than an installed package, so the reference targets that shipped
 * `dist/ambient.d.ts` directly; an app that installs `@treaty/jsx` as a real
 * dependency uses the package-name form instead — either
 * `/// <reference types="@treaty/jsx/ambient" />` or
 * `compilerOptions.types: ["@treaty/jsx/ambient"]`.
 */

declare module '@module-federation/enhanced/runtime' {
	/** A runtime remote definition the plugin may repoint by name. */
	export interface FederationRuntimeRemote {
		readonly name?: string
		readonly alias?: string
		readonly entry?: string
		readonly [key: string]: unknown
	}
	/** The named runtime-plugin object `registerPlugins`/`init` accept. */
	export interface FederationRuntimePlugin {
		readonly name: string
		readonly [hook: string]: unknown
	}
}

declare module '@module-federation/enhanced' {
	export class ModuleFederationPlugin {
		constructor(options: unknown)
		apply(compiler: unknown): void
	}
}

declare module '@module-federation/enhanced/rspack' {
	export class ModuleFederationPlugin {
		constructor(options: unknown)
		apply(compiler: unknown): void
	}
}

declare module '@rsbuild/core' {
	/** An Rsbuild plugin: a named object with a `setup(api)` entry point. */
	export interface RsbuildPlugin {
		readonly name: string
		setup(api: unknown): void | Promise<void>
	}
}
