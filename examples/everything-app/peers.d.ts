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

/**
 * Authoring-format module shapes for the `.treaty` SFC and `.tjsx` JSX surfaces.
 *
 * tsgo cannot parse these authoring extensions directly (the Treaty compiler
 * lowers them to Ivy components at build time), so importing one from a plain
 * `.ts` host would otherwise be an unresolved module. These ambient
 * declarations restate what the compiler emits: each authoring file
 * default-exports a component VALUE that selectorless auto-import consumes by
 * reference. The real lowered component is an assignable superset; nothing here
 * runs. The shipped `@treaty/jsx` types still own intrinsic-element typing for
 * the `.tjsx`/`.tsx` sources themselves.
 */
declare module '*.treaty' {
	/**
	 * The lowered, selectorless standalone component the compiler emits. Typed as
	 * a constructable so it is assignable to Angular's `imports: Type<unknown>[]`
	 * and usable as a `loadComponent` target; the concrete shape is synthesized.
	 */
	const component: new (...args: never[]) => unknown
	export default component
}

declare module '*.tjsx' {
	/**
	 * The lowered, selectorless standalone component the compiler emits. Typed as
	 * a constructable so it is assignable to Angular's `imports: Type<unknown>[]`
	 * and usable as a `loadComponent` target; the concrete shape is synthesized.
	 */
	const component: new (...args: never[]) => unknown
	export default component
}
