/**
 * @module
 *
 * CI affected-change detection over Treaty's federated module graph. Federation
 * is Treaty's unit of **deployment granularity**: the compiler emits the host,
 * every lazy feature route, and every library as an independently versioned,
 * deployable, rollback-able module. CI should only compile, test, and deploy the
 * modules that a given change actually touches — not the whole app.
 *
 * The core is {@link computeAffectedModules}: given a set of changed file paths
 * and a {@link ModuleDependencyGraph} (`moduleId -> { files, dependsOn }`), it
 * returns the set of modules that changed. A module is affected when **one of its
 * own files changed**, OR when a module it `dependsOn` (transitively) is
 * affected — this is the shared-lib fan-out: editing a shared library marks
 * every route/lib that depends on it, directly or through other libs.
 *
 * The computation is pure and deterministic: same inputs, same sorted output. It
 * makes no assumptions about path format (it compares the strings it is given),
 * so callers can pass repo-relative or absolute paths as long as they are
 * consistent with the graph's `files`.
 */

/**
 * One node in the federated module dependency graph: the source `files` that
 * make up the module, and the `moduleId`s it `dependsOn`. `dependsOn` points at
 * the modules whose code this module consumes (e.g. a route that imports a shared
 * lib lists that lib). Edges are directed consumer -> dependency; affectedness
 * propagates the opposite way (a changed dependency marks its consumers).
 */
export interface ModuleNode {
	/** Source files that belong to this module. Compared verbatim against changed paths. */
	readonly files: readonly string[]
	/** Module ids this module directly depends on (consumes the code of). */
	readonly dependsOn: readonly string[]
}

/**
 * The federated module dependency graph: every `moduleId` (host, route remotes,
 * libs) mapped to its {@link ModuleNode}. This is the graph CI walks to decide
 * what a change affects. A `dependsOn` entry that is not itself a key is a
 * dangling edge; see {@link ComputeAffectedOptions.onMissingDependency}.
 */
export type ModuleDependencyGraph = Readonly<Record<string, ModuleNode>>

/** Options for {@link computeAffectedModules}. */
export interface ComputeAffectedOptions {
	/**
	 * How to match a changed file path against a module's `files`. Defaults to
	 * exact string equality. Supply a custom matcher to, e.g., treat a directory
	 * prefix as owning every file beneath it.
	 */
	readonly matchFile?: (changedPath: string, moduleFile: string) => boolean
	/**
	 * What to do when a `dependsOn` edge names a module the graph has no node for.
	 *   - `'ignore'` (default): skip the dangling edge.
	 *   - `'error'`: throw, surfacing a malformed graph in CI early.
	 */
	readonly onMissingDependency?: 'ignore' | 'error'
}

/**
 * Compute the set of federated modules a change actually affects.
 *
 * A module is **affected** if either:
 *   1. one of its own `files` matches a changed path (a direct edit), or
 *   2. any module it `dependsOn` is affected — applied transitively, so a change
 *      to a shared lib fans out to every route/lib that (directly or through
 *      intermediate libs) depends on it.
 *
 * Pure and deterministic: the returned array is sorted and contains each
 * affected `moduleId` exactly once. Modules absent from the result need no
 * recompile/retest/redeploy for this change.
 *
 * @param changedFiles The paths that changed (a set or any iterable of strings).
 * @param graph The module dependency graph to walk.
 * @param options See {@link ComputeAffectedOptions}.
 * @returns The affected `moduleId`s, sorted ascending.
 * @throws if `onMissingDependency: 'error'` and a `dependsOn` edge is dangling,
 *   or if a `dependsOn` graph has a cycle (Treaty's module graph is a DAG).
 */
export function computeAffectedModules(
	changedFiles: Iterable<string>,
	graph: ModuleDependencyGraph,
	options: ComputeAffectedOptions = {}
): string[] {
	const matchFile = options.matchFile ?? defaultMatchFile
	const onMissingDependency = options.onMissingDependency ?? 'ignore'

	const changed = changedFiles instanceof Set ? changedFiles : new Set(changedFiles)
	const moduleIds = Object.keys(graph)

	// Validate dependsOn edges up front so 'error' mode surfaces a bad graph even
	// for modules that turn out not to be affected.
	if (onMissingDependency === 'error') {
		for (const id of moduleIds) {
			for (const dep of graph[id]!.dependsOn) {
				if (!(dep in graph)) {
					throw new RangeError(
						`computeAffectedModules: module "${id}" dependsOn unknown module "${dep}"`
					)
				}
			}
		}
	}

	// Memoize each module's affectedness; a cycle is detected via the in-progress
	// marker so a malformed (non-DAG) graph throws rather than spinning.
	const enum State {
		Unknown,
		InProgress,
		Affected,
		Unaffected,
	}
	const state = new Map<string, State>()

	const isAffected = (id: string): boolean => {
		const seen = state.get(id)
		if (seen === State.Affected) return true
		if (seen === State.Unaffected) return false
		if (seen === State.InProgress) {
			throw new RangeError(`computeAffectedModules: dependency cycle detected at module "${id}"`)
		}

		state.set(id, State.InProgress)

		const node = graph[id]!

		// 1. A direct edit to one of this module's own files.
		let affected = false
		for (const file of node.files) {
			for (const changedPath of changed) {
				if (matchFile(changedPath, file)) {
					affected = true
					break
				}
			}
			if (affected) break
		}

		// 2. Fan-out: affected if any dependency is (transitively) affected.
		if (!affected) {
			for (const dep of node.dependsOn) {
				if (!(dep in graph)) {
					// 'error' mode already threw above; here we ignore dangling edges.
					continue
				}
				if (isAffected(dep)) {
					affected = true
					break
				}
			}
		}

		state.set(id, affected ? State.Affected : State.Unaffected)
		return affected
	}

	const result: string[] = []
	for (const id of moduleIds) {
		if (isAffected(id)) result.push(id)
	}
	result.sort()
	return result
}

/** Default file matcher for {@link computeAffectedModules}: exact string equality. */
function defaultMatchFile(changedPath: string, moduleFile: string): boolean {
	return changedPath === moduleFile
}
