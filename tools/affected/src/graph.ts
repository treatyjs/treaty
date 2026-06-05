/**
 * @module
 *
 * The **project dependency graph** the affected-set computation runs over.
 *
 * A Treaty app's deployment granularity is the *federated module*: the host
 * container, every lazy feature route (a route-as-remote), and every workspace
 * library — each independently versioned, deployable, and rollback-able (see
 * `@treaty/module-federation`'s `federatedModules`). This module models that set
 * of modules PLUS the one thing the federation package deliberately does not: the
 * **edges between them** — which route/host depends on which lib, which lib
 * depends on which other lib. Those edges are what make a shared-lib change *fan
 * out* to its dependents.
 *
 * The graph is a plain, serializable description (it round-trips through JSON, so
 * CI can hand it in from a file or stdin):
 *   - {@link ProjectNode} — one module: a stable `id`, its `kind`
 *     (`host`/`route`/`lib`), the source `paths` it OWNS (the file prefixes a
 *     changed file is attributed to), and the module ids it `dependsOn`.
 *   - {@link ProjectGraph} — the set of nodes, indexed and validated into a form
 *     the affected walk consumes ({@link buildGraph}).
 *
 * The path-ownership model is intentionally simple and deterministic: a node owns
 * a set of normalized path PREFIXES, and a changed file belongs to the node whose
 * **longest** owning prefix matches it (so `libs/ui/button` wins over `libs/ui`
 * when both are nodes). This is the same "nearest project owns the file" rule
 * Nx/Turborepo use, reduced to its deterministic core.
 */

/**
 * The classification of a federated module — identical to
 * `@treaty/module-federation`'s `FederatedModuleKind`. The host container, a lazy
 * feature route (route-as-remote), or a shared library. The kind is carried
 * through to the affected output so CI can scope per-kind (e.g. redeploy only
 * affected `route`/`lib` remotes, rebuild the `host` only when it is affected).
 */
export type ProjectKind = 'host' | 'route' | 'lib'

/**
 * One node in the project graph: an independently deployable federated module and
 * the source it owns + depends on.
 */
export interface ProjectNode {
	/**
	 * Stable identity of the module across versions — the federation exposes key
	 * for a route/lib, or the host name for the host. The same identity the
	 * deployment manifest versions, deploys, and rolls back.
	 */
	readonly id: string
	/** Whether this module is the host container, a lazy route, or a library. */
	readonly kind: ProjectKind
	/**
	 * The source path PREFIXES this module owns, relative to the repo root, using
	 * forward slashes. A changed file is attributed to the node with the longest
	 * matching owning prefix. A node may own several (e.g. a route that lives in
	 * both `src/app/dashboard` and a co-located `src/app/dashboard-shared`).
	 */
	readonly paths: readonly string[]
	/**
	 * The ids of the modules this module directly depends on (its imports of other
	 * federated modules / libs). A change to a dependency fans out to this module.
	 * Defaults to none.
	 */
	readonly dependsOn?: readonly string[]
}

/** The serializable project graph: just its set of nodes. */
export interface ProjectGraphInput {
	readonly nodes: readonly ProjectNode[]
}

/**
 * A validated, indexed project graph ready for the affected walk. Built by
 * {@link buildGraph}; do not construct directly.
 */
export interface ProjectGraph {
	/** Nodes keyed by id, in a stable (sorted-by-id) Map. */
	readonly nodes: ReadonlyMap<string, ProjectNode>
	/**
	 * Reverse adjacency: for each module id, the ids that DEPEND on it (its
	 * dependents). This is the edge set the affected walk follows — a change to
	 * `x` reaches everything reachable from `x` here.
	 */
	readonly dependents: ReadonlyMap<string, readonly string[]>
	/**
	 * Owning prefixes sorted longest-first, each paired with its owning node id.
	 * The first entry whose prefix matches a changed file wins (longest match).
	 */
	readonly ownership: readonly OwnedPath[]
}

/** A single owned path prefix paired with the node that owns it. */
export interface OwnedPath {
	/** Normalized, forward-slash, no trailing-slash path prefix. */
	readonly prefix: string
	/** The id of the node that owns this prefix. */
	readonly nodeId: string
}

/** An error describing why a {@link ProjectGraphInput} is not a valid graph. */
export class GraphError extends Error {
	override readonly name = 'GraphError'
}

/**
 * Normalize a path for ownership matching: forward slashes, no leading `./`, no
 * leading or trailing slashes, collapsed `//`. Returns `''` for a path that
 * normalizes to empty (an invalid owning prefix).
 */
export function normalizePath(path: string): string {
	return path
		.replace(/\\/g, '/')
		.replace(/\/{2,}/g, '/')
		.replace(/^\.\//, '')
		.replace(/^\/+|\/+$/g, '')
}

/**
 * Decide whether the normalized changed-file path `file` is owned by the
 * normalized owning `prefix`. A file is owned when it equals the prefix exactly
 * (the prefix is the file itself) or sits under it on a path boundary (so
 * `libs/ui` owns `libs/ui/button.ts` but NOT `libs/ui-kit/x.ts`).
 */
export function isUnderPrefix(file: string, prefix: string): boolean {
	if (prefix === '') return false
	return file === prefix || file.startsWith(`${prefix}/`)
}

/**
 * Validate a {@link ProjectGraphInput} and build the indexed {@link ProjectGraph}
 * the affected walk consumes. Deterministic: nodes, dependents, and ownership are
 * all sorted, so the same input always yields byte-identical traversal order.
 *
 * Throws {@link GraphError} on a malformed graph: a missing/blank id, a duplicate
 * id, a node with no owning paths, a blank owning path, or a `dependsOn` edge to
 * an id that is not a node. Failing loudly here keeps the affected output
 * trustworthy — CI should never silently scope to the wrong set because a graph
 * edge pointed at nothing.
 */
export function buildGraph(input: ProjectGraphInput): ProjectGraph {
	// The input commonly arrives as parsed JSON, so validate defensively rather
	// than trusting the static type. Treat each field as `unknown` and prove it.
	const rawNodes: unknown = (input as { nodes?: unknown } | null | undefined)?.nodes
	if (!input || !Array.isArray(rawNodes)) {
		throw new GraphError('graph must be an object with a `nodes` array')
	}

	const nodes = new Map<string, ProjectNode>()
	for (const raw of rawNodes as readonly unknown[]) {
		const node = (raw ?? {}) as {
			id?: unknown
			kind?: unknown
			paths?: unknown
			dependsOn?: unknown
		}
		const id = typeof node.id === 'string' ? node.id.trim() : ''
		if (id === '') {
			throw new GraphError('every node needs a non-empty string `id`')
		}
		if (nodes.has(id)) {
			throw new GraphError(`duplicate node id: ${id}`)
		}
		if (node.kind !== 'host' && node.kind !== 'route' && node.kind !== 'lib') {
			throw new GraphError(`node ${id} has invalid kind: ${String(node.kind)}`)
		}
		if (!Array.isArray(node.paths) || node.paths.length === 0) {
			throw new GraphError(`node ${id} must own at least one path`)
		}
		const paths = (node.paths as readonly unknown[]).map((p) => normalizePath(String(p)))
		if (paths.some((p) => p === '')) {
			throw new GraphError(`node ${id} has an empty owning path`)
		}
		const dependsOn = (Array.isArray(node.dependsOn) ? node.dependsOn : []).map((d) =>
			String(d)
		)
		nodes.set(id, { id, kind: node.kind, paths, dependsOn })
	}

	// Validate edges point at real nodes (after every node is known so order
	// in the input does not matter).
	for (const node of nodes.values()) {
		for (const dep of node.dependsOn ?? []) {
			if (!nodes.has(dep)) {
				throw new GraphError(`node ${node.id} depends on unknown module: ${dep}`)
			}
			if (dep === node.id) {
				throw new GraphError(`node ${node.id} depends on itself`)
			}
		}
	}

	// Stable, sorted-by-id node map.
	const sortedNodes = new Map<string, ProjectNode>(
		[...nodes.entries()].sort(([a], [b]) => compare(a, b))
	)

	// Reverse adjacency: dependents[dep] includes every node that dependsOn dep.
	const dependentSets = new Map<string, Set<string>>()
	for (const id of sortedNodes.keys()) dependentSets.set(id, new Set())
	for (const node of sortedNodes.values()) {
		for (const dep of node.dependsOn ?? []) {
			dependentSets.get(dep)!.add(node.id)
		}
	}
	const dependents = new Map<string, readonly string[]>()
	for (const [id, set] of dependentSets) {
		dependents.set(id, [...set].sort(compare))
	}

	// Ownership table, longest prefix first (ties broken by id for determinism).
	const ownership: OwnedPath[] = []
	for (const node of sortedNodes.values()) {
		for (const prefix of node.paths) {
			ownership.push({ prefix, nodeId: node.id })
		}
	}
	ownership.sort((a, b) => {
		if (b.prefix.length !== a.prefix.length) return b.prefix.length - a.prefix.length
		if (a.prefix !== b.prefix) return compare(a.prefix, b.prefix)
		return compare(a.nodeId, b.nodeId)
	})

	return { nodes: sortedNodes, dependents, ownership }
}

/** Deterministic string compare (no locale surprises). */
function compare(a: string, b: string): number {
	return a < b ? -1 : a > b ? 1 : 0
}
