/**
 * @module
 *
 * `@treaty/federation-deploy` — the versioned manifest + runtime that makes
 * Treaty's federated modules independently deployable and rollback-able.
 *
 * Treaty compiles every lazy feature route and library into its own federated
 * module (see `@treaty/module-federation`). This package is the **deployment
 * granularity** layer on top of that: a {@link FederationManifest} records, for
 * every `moduleId` (the host, each route remote, each lib), which **version** is
 * live and the **url** it is served from. Treaty is a compiler — it emits the
 * modules and the manifest; the serving platform owns the manifest and flips it.
 *
 * Three layers, smallest API surface first:
 *   - {@link buildManifest} / {@link serializeManifest} / {@link parseManifest} —
 *     create and round-trip the manifest as JSON (a committed deploy artifact).
 *   - {@link setModuleVersion} (deploy) / {@link rollbackModule} (rollback) —
 *     pure helpers that return a NEW manifest with exactly one module repointed,
 *     leaving every other module untouched, so a single route/lib is rolled
 *     forward or back without redeploying the app.
 *   - {@link createTreatyMfRuntimePlugin} — a `@module-federation/enhanced`
 *     runtime plugin factory that resolves each remote's current url+version from
 *     the manifest at load, so a manifest flip takes effect on the next load.
 *
 * On top of the manifest sits the **CI deployment** layer, which lets a pipeline
 * compile/test/deploy ONLY the modules a change touches:
 *   - {@link computeAffectedModules} — pure graph computation over a
 *     `moduleId -> { files, dependsOn }` graph: a module is affected by a direct
 *     file edit or by (transitive) shared-lib fan-out.
 *   - {@link DeployPlugin} / {@link DeployPluginRegistry} — a pluggable deploy
 *     method interface plus a name-keyed registry, with reference
 *     {@link NoopDeployPlugin} and {@link FsDeployPlugin} implementations.
 *   - {@link affectedDeployPlan} — the CI entry point: from a change set it
 *     returns which modules to compile/test/deploy and the resulting manifest
 *     deltas.
 *
 * The `@module-federation/enhanced/runtime` peer is referenced structurally, so
 * this package typechecks and the manifest helpers run without it installed.
 */

export {
	buildManifest,
	serializeManifest,
	parseManifest,
	getModule,
	setModuleVersion,
	rollbackModule,
	MANIFEST_SCHEMA,
} from './manifest.js'
export type {
	FederationManifest,
	ModuleDeployment,
	ModuleKind,
	FederatedModuleInput,
	BuildManifestOptions,
	SetModuleVersionOptions,
} from './manifest.js'

export { createTreatyMfRuntimePlugin } from './runtime.js'
export type {
	ManifestSource,
	ModuleIdResolver,
	TreatyMfRuntimePluginOptions,
	TreatyMfRuntimePluginFactory,
} from './runtime.js'

export { computeAffectedModules } from './affected.js'
export type {
	ModuleNode,
	ModuleDependencyGraph,
	ComputeAffectedOptions,
} from './affected.js'

export {
	DeployPluginRegistry,
	NoopDeployPlugin,
	FsDeployPlugin,
} from './plugin.js'
export type {
	DeployPlugin,
	DeployModule,
	DeployArtifact,
	DeployContext,
	FsDeployPluginOptions,
} from './plugin.js'

export { affectedDeployPlan } from './plan.js'
export type {
	AffectedDeployPlan,
	AffectedDeployPlanOptions,
	PlannedDeploy,
	ManifestChange,
	VersionPolicy,
} from './plan.js'
