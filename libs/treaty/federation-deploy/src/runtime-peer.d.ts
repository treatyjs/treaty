/**
 * Ambient declaration for the optional `@module-federation/enhanced/runtime`
 * peer. `@treaty/federation-deploy` produces a runtime plugin shaped for this
 * package's `registerPlugins` / `init` API, but must typecheck (and the pure
 * manifest helpers must run) without the peer installed. The real package's
 * `FederationRuntimePlugin` is a structural superset of the subset declared
 * here, so what we emit remains assignable to it.
 *
 * Only the hooks Treaty's deploy plugin uses are modelled:
 *   - `beforeRequest` — rewrite the remote (its `entry` url) before a module is
 *     requested, so the manifest's CURRENT url+version wins at load time;
 *   - `resolveRemote` — supply/repoint a remote definition by name.
 * Extra fields the runtime carries are accepted via the index signature.
 */
declare module '@module-federation/enhanced/runtime' {
	/**
	 * A federation runtime remote, in the two forms the host accepts: a
	 * `name@entry` URL string (`version`/`entry` form) or an explicit object.
	 * Treaty always resolves to the object form so it can carry the version.
	 */
	export interface FederationRuntimeRemote {
		name: string
		entry?: string
		version?: string
		alias?: string
		[extra: string]: unknown
	}

	/** Argument bag passed to the `beforeRequest` hook. */
	export interface FederationBeforeRequestArgs {
		id: string
		options: {
			remotes: FederationRuntimeRemote[]
			[extra: string]: unknown
		}
		[extra: string]: unknown
	}

	/** Argument bag passed to the `resolveRemote` hook. */
	export interface FederationResolveRemoteArgs {
		remote: FederationRuntimeRemote
		[extra: string]: unknown
	}

	/**
	 * A federation runtime plugin: a named object whose hook functions are
	 * invoked by the host at the corresponding lifecycle points. Hooks return
	 * (a possibly-mutated copy of) their argument bag.
	 */
	export interface FederationRuntimePlugin {
		name: string
		beforeRequest?: (
			args: FederationBeforeRequestArgs
		) => FederationBeforeRequestArgs | Promise<FederationBeforeRequestArgs>
		resolveRemote?: (
			args: FederationResolveRemoteArgs
		) => FederationRuntimeRemote | Promise<FederationRuntimeRemote>
		[hook: string]: unknown
	}

	export function registerPlugins(plugins: FederationRuntimePlugin[]): void
	export function init(options: unknown): unknown
	export function loadRemote<T = unknown>(id: string): Promise<T>
}
