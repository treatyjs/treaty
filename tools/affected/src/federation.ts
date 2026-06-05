/**
 * @module
 *
 * Bridge from `@treaty/module-federation`'s federated-module list to this tool's
 * {@link ProjectGraphInput}.
 *
 * `@treaty/module-federation` already enumerates the federated **modules** a
 * Treaty app deploys — the host, every lazy route-as-remote, and every lib — via
 * `federatedModules(...)`, each carrying a stable `moduleId`, a `kind`, and the
 * local `path` it is backed by. That is exactly the *node* half of our graph. The
 * one thing that package deliberately does NOT model is the *edge* half — which
 * module depends on which — because federation exposes are derived per-module
 * with no cross-module dependency analysis.
 *
 * So this bridge consumes the federation module list READ-ONLY (it is typed
 * structurally — {@link FederatedModuleLike} — so we neither import nor build the
 * package; CI passes the JSON `federatedModules()` returns) and layers the
 * dependency edges on top from a separate {@link DependencyEdges} input. The
 * result is a {@link ProjectGraphInput} ready for {@link buildGraph}. When the
 * edges are omitted you still get a valid graph of independent modules (a change
 * to one affects only itself), which is the correct conservative default until
 * real cross-module dependencies are supplied.
 */

import type { ProjectGraphInput, ProjectKind, ProjectNode } from './graph.js'

/**
 * The structural shape of one entry from `@treaty/module-federation`'s
 * `federatedModules(...)` (its `FederatedModule`). Typed here so this tool needs
 * no dependency on — and no build of — that package: CI serializes the array to
 * JSON and hands it in.
 */
export interface FederatedModuleLike {
	/** The federation exposes key (route/lib) or host name — the module's id. */
	readonly moduleId: string
	/** Whether this is the host, a lazy route, or a lib. */
	readonly kind: ProjectKind
	/** Local module path backing the module (the source it owns). */
	readonly path: string
}

/**
 * The dependency edges to layer onto the federation module list — the cross-module
 * `dependsOn` relationships the federation package does not derive. Keyed by
 * `moduleId`; each value is the list of `moduleId`s that module directly depends
 * on. A module absent from the map (or mapped to `[]`) depends on nothing.
 */
export type DependencyEdges = Readonly<Record<string, readonly string[]>>

/** Options for {@link graphFromFederation}. */
export interface FederationGraphOptions {
	/**
	 * Cross-module dependency edges keyed by `moduleId`. Layered onto the modules
	 * so a shared-lib change fans out to its dependents. Optional — omit for a
	 * graph of independent modules.
	 */
	readonly dependencies?: DependencyEdges
	/**
	 * Extra owning path prefixes per `moduleId`, merged with each module's own
	 * `path`. Use when a module owns source beyond its backing path (e.g. a route
	 * that also owns a co-located fixtures dir). Keyed by `moduleId`.
	 */
	readonly extraPaths?: Readonly<Record<string, readonly string[]>>
}

/**
 * Turn a `@treaty/module-federation` `federatedModules(...)` result into a
 * {@link ProjectGraphInput}, layering on the supplied cross-module dependency
 * edges. Each federated module becomes one node owning its backing `path` (plus
 * any {@link FederationGraphOptions.extraPaths}); its `dependsOn` comes from
 * {@link FederationGraphOptions.dependencies}.
 *
 * Validation of the edges (every `dependsOn` must point at a real module) is left
 * to {@link buildGraph}, so a typo in the edge map fails loudly rather than
 * silently scoping CI to the wrong set.
 */
export function graphFromFederation(
	modules: readonly FederatedModuleLike[],
	options: FederationGraphOptions = {}
): ProjectGraphInput {
	const dependencies = options.dependencies ?? {}
	const extraPaths = options.extraPaths ?? {}

	const nodes: ProjectNode[] = modules.map((module) => {
		const own = [module.path, ...(extraPaths[module.moduleId] ?? [])]
		return {
			id: module.moduleId,
			kind: module.kind,
			paths: own,
			dependsOn: dependencies[module.moduleId] ?? [],
		}
	})

	return { nodes }
}
