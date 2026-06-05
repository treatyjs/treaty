/**
 * @module
 *
 * The **affected-set computation** — Nx/Turborepo "affected" reduced to its
 * deterministic core, at federated-module granularity.
 *
 * Given a list of changed files (e.g. `git diff --name-only main...HEAD`) and a
 * {@link ProjectGraph}, it answers the one question CI needs to scope a build:
 * *which federated modules must be re-compiled, re-tested, and re-deployed?*
 *
 * The answer is two steps, both deterministic:
 *   1. **Attribute** each changed file to the module that OWNS it (the node with
 *      the longest matching owning prefix) → the set of *directly changed*
 *      modules. Files owned by no module are reported separately as
 *      {@link AffectedResult.unmatchedFiles} (a changed root config, CI script,
 *      etc.) so the caller decides whether they force a full build.
 *   2. **Fan out** along the reverse-dependency edges: a changed module's
 *      transitive **dependents** are affected too (a shared-lib change reaches
 *      every route/host that imports it). The closure includes the directly
 *      changed modules themselves.
 *
 * The result is the precise set of modules to act on — a leaf change yields just
 * that leaf, a shared-lib change yields the lib plus all its dependents, and an
 * unrelated change yields nothing (no module owns the file) — so CI compiles only
 * that federation, tests only that part, and deploys only that part.
 */

import { buildGraph, isUnderPrefix, normalizePath } from './graph.js'
import type { ProjectGraph, ProjectGraphInput, ProjectKind, ProjectNode } from './graph.js'

/** One affected module in the result, carrying why it is affected. */
export interface AffectedModule {
	/** The module's stable id. */
	readonly id: string
	/** The module's kind (host / route / lib). */
	readonly kind: ProjectKind
	/**
	 * `true` when a changed file is owned directly by this module; `false` when it
	 * is affected only transitively (a dependency of it changed). Lets CI tell
	 * "rebuild because its own source changed" from "rebuild because a lib it
	 * consumes changed".
	 */
	readonly directlyChanged: boolean
}

/** The full result of an affected-set computation. */
export interface AffectedResult {
	/**
	 * The affected modules — the directly changed modules plus their transitive
	 * dependents — sorted by id for deterministic output. This is the set CI
	 * scopes compile/test/deploy to.
	 */
	readonly affected: readonly AffectedModule[]
	/**
	 * Just the affected module ids, sorted. The flat list CI usually wants
	 * (`--projects` / `--filter` arguments, a deploy include-list).
	 */
	readonly affectedIds: readonly string[]
	/** The ids of modules a changed file is owned by directly, sorted. */
	readonly directlyChangedIds: readonly string[]
	/**
	 * Changed files attributed to no module, sorted. A root/CI/tooling change that
	 * is not inside any federated module's owned paths. The caller decides whether
	 * these force a full build; the affected walk itself never invents modules for
	 * them.
	 */
	readonly unmatchedFiles: readonly string[]
}

/** Options for {@link computeAffected}. */
export interface AffectedOptions {
	/**
	 * Treat the listed files as "owned by everything": when ANY changed file
	 * matches one of these normalized prefixes, every module is affected. Use for
	 * global inputs (lockfile, root tsconfig, the CI workflow itself) whose change
	 * could plausibly impact any module. Defaults to none — a global change is
	 * otherwise reported in {@link AffectedResult.unmatchedFiles} and affects
	 * nothing on its own.
	 */
	readonly globalTriggers?: readonly string[]
}

/**
 * Attribute a single normalized changed file to the module that owns it, or
 * `undefined` when no module's owning prefix matches. Longest matching prefix
 * wins (the ownership table is pre-sorted longest-first by {@link buildGraph}).
 */
function ownerOf(graph: ProjectGraph, file: string): string | undefined {
	for (const owned of graph.ownership) {
		if (isUnderPrefix(file, owned.prefix)) return owned.nodeId
	}
	return undefined
}

/**
 * Compute the transitive dependent closure of a set of seed module ids over the
 * reverse-dependency edges: the seeds plus everything that (transitively) depends
 * on a seed. Iterative breadth-first walk — no recursion, so a deep graph cannot
 * blow the stack, and a cyclic graph terminates (visited guard).
 */
function dependentClosure(
	graph: ProjectGraph,
	seeds: Iterable<string>
): Set<string> {
	const visited = new Set<string>()
	const queue: string[] = []
	for (const seed of seeds) {
		if (!visited.has(seed)) {
			visited.add(seed)
			queue.push(seed)
		}
	}
	while (queue.length > 0) {
		const current = queue.shift()!
		for (const dependent of graph.dependents.get(current) ?? []) {
			if (!visited.has(dependent)) {
				visited.add(dependent)
				queue.push(dependent)
			}
		}
	}
	return visited
}

/** Deterministic string compare. */
function compare(a: string, b: string): number {
	return a < b ? -1 : a > b ? 1 : 0
}

/**
 * Compute the affected set for a changed-file list over an already-built
 * {@link ProjectGraph}.
 *
 * Steps: normalize the changed files, attribute each to its owning module (or to
 * `unmatchedFiles`), honor any {@link AffectedOptions.globalTriggers} (a matching
 * global file makes every module directly changed), then take the transitive
 * dependent closure of the directly-changed set. The output is fully sorted and
 * deterministic.
 */
export function computeAffected(
	graph: ProjectGraph,
	changedFiles: readonly string[],
	options: AffectedOptions = {}
): AffectedResult {
	const globalTriggers = (options.globalTriggers ?? []).map(normalizePath).filter(Boolean)

	const directlyChanged = new Set<string>()
	const unmatched = new Set<string>()
	let globalHit = false

	for (const raw of changedFiles) {
		const file = normalizePath(String(raw))
		if (file === '') continue

		if (globalTriggers.some((prefix) => isUnderPrefix(file, prefix) || file === prefix)) {
			globalHit = true
			continue
		}

		const owner = ownerOf(graph, file)
		if (owner === undefined) {
			unmatched.add(file)
		} else {
			directlyChanged.add(owner)
		}
	}

	// A global-trigger change affects every module directly.
	if (globalHit) {
		for (const id of graph.nodes.keys()) directlyChanged.add(id)
	}

	const affectedIds = dependentClosure(graph, directlyChanged)

	const affected: AffectedModule[] = [...affectedIds]
		.sort(compare)
		.map((id) => {
			const node = graph.nodes.get(id) as ProjectNode
			return {
				id,
				kind: node.kind,
				directlyChanged: directlyChanged.has(id),
			}
		})

	return {
		affected,
		affectedIds: affected.map((m) => m.id),
		directlyChangedIds: [...directlyChanged].sort(compare),
		unmatchedFiles: [...unmatched].sort(compare),
	}
}

/**
 * Convenience: build the graph from a raw {@link ProjectGraphInput} and compute
 * the affected set in one call. Throws `GraphError` if the graph is malformed
 * (see {@link buildGraph}).
 */
export function affectedFromInput(
	input: ProjectGraphInput,
	changedFiles: readonly string[],
	options: AffectedOptions = {}
): AffectedResult {
	return computeAffected(buildGraph(input), changedFiles, options)
}
