/// <reference path="../../libs/treaty/compiler/dist/ambient.d.ts" />
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
 * The authoring-format module shims are NOT declared here: each ships from the
 * package that OWNS the format — `*.treaty` from `@treaty/compiler` (its
 * `./ambient` entry) and `*.tjsx` from `@treaty/jsx` (its `./ambient` entry) —
 * picked up by the two triple-slash references above, so no per-app
 * `declare module` block is needed. This workspace consumes the libs from their
 * built `dist` rather than installed packages, so the references target the
 * shipped `dist/ambient.d.ts` files directly; an app that installs them as real
 * dependencies uses the package-name form instead — either
 * `/// <reference types="@treaty/compiler/ambient" />` /
 * `"@treaty/jsx/ambient"`, or `compilerOptions.types`.
 */

/**
 * Side-effect CSS imports (`import './styles.css'`). Vite handles `.css`
 * natively (the dev server injects it; the build emits it as an asset) and
 * `@treaty/vite` does not claim `.css`, so a CSS import has no runtime exports —
 * it only needs an ambient module declaration so `tsgo` accepts the side-effect
 * import. This app sets `compilerOptions.types: []`, so Vite's own
 * `vite/client` `*.css` declaration is not in scope; restate the minimal shape
 * here (an empty default export, matching Vite's CSS module type).
 */
declare module '*.css' {
	const css: string
	export default css
}

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
