/**
 * @module
 *
 * `@treaty-tools/affected` — Nx/Turborepo-style **affected** at federated-module
 * granularity for Treaty.
 *
 * Treaty's unit of deployment is the federated module: the host, every lazy
 * route-as-remote, and every workspace lib, each independently versioned and
 * deployable (see `@treaty/module-federation`). When a commit changes some files,
 * CI should not rebuild/test/redeploy the whole app — only the modules actually
 * affected: the modules whose source changed, PLUS every module that transitively
 * depends on them (a shared-lib change fans out to its dependents).
 *
 * This package computes exactly that set, deterministically and with no AI:
 *   - {@link buildGraph} / {@link ProjectGraphInput} — the project dependency
 *     graph (federated modules + the cross-module edges federation does not model).
 *   - {@link computeAffected} / {@link affectedFromInput} — the affected-set walk:
 *     attribute changed files to owning modules, then take the transitive
 *     dependent closure.
 *   - {@link graphFromFederation} — seed the node set read-only from
 *     `@treaty/module-federation`'s `federatedModules(...)`, layering edges on top.
 *   - {@link parseChangedFiles} / {@link gitChangedFiles} — the changed-file source.
 *   - {@link run} — the CLI CI invokes (`treaty-affected`, see `bin/affected.mjs`).
 */

export {
	buildGraph,
	normalizePath,
	isUnderPrefix,
	GraphError,
} from './graph.js'
export type {
	ProjectKind,
	ProjectNode,
	ProjectGraphInput,
	ProjectGraph,
	OwnedPath,
} from './graph.js'

export { computeAffected, affectedFromInput } from './affected.js'
export type {
	AffectedModule,
	AffectedResult,
	AffectedOptions,
} from './affected.js'

export { graphFromFederation } from './federation.js'
export type {
	FederatedModuleLike,
	DependencyEdges,
	FederationGraphOptions,
} from './federation.js'

export { parseChangedFiles, gitChangedFiles } from './git.js'
export type { GitChangedFilesOptions } from './git.js'

export { run, main, parseArgs, CliError } from './cli.js'
export type { CliOptions, CliOutput, CliIo } from './cli.js'
