/**
 * Ambient declaration for the optional `@module-federation/enhanced` peer so
 * `@treaty/rspack` typechecks (and the base loader/plugin run) without it
 * installed. The `ModuleFederationPlugin` is loaded only when auto-MF is on.
 * The real package's export is assignable to this structural shape.
 */
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
