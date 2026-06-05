/**
 * @module
 *
 * The CI entry point that ties affected-change detection to the deploy layer.
 * {@link affectedDeployPlan} answers the only question a federated-deploy CI run
 * needs to ask: *given what changed, which modules do I compile, test, and
 * deploy, and what does that do to the manifest?* It is pure planning — it reads
 * the change set, the module graph, and the current manifest, and returns a
 * {@link AffectedDeployPlan} describing the work and the resulting manifest
 * deltas, without performing any I/O or running any {@link DeployPlugin}.
 *
 * The plan is deployment-granularity in action: only the modules a change
 * actually touches (a direct edit, or shared-lib fan-out via the graph) are
 * scheduled. The orchestrator then walks {@link AffectedDeployPlan.deploy},
 * invokes a plugin per module, and applies the {@link AffectedDeployPlan.manifestChanges}
 * to flip the live manifest — or skips entirely when nothing is affected.
 */

import {
	computeAffectedModules,
	type ComputeAffectedOptions,
	type ModuleDependencyGraph,
} from './affected.js'
import type { FederationManifest, ModuleDeployment } from './manifest.js'

/**
 * The version a planned module should be deployed at. The planner does not invent
 * versions (CI owns version policy); supply one of:
 *   - a constant `string` applied to every affected module, or
 *   - a function `(moduleId, previous) => version` to derive per module from its
 *     current {@link ModuleDeployment} (e.g. bump a build id).
 * When omitted, the plan still reports what to compile/test/deploy but leaves
 * {@link PlannedDeploy.version} as the module's current version (a re-deploy of
 * the same version) — useful for "rebuild the affected set" runs.
 */
export type VersionPolicy = string | ((moduleId: string, previous: ModuleDeployment | undefined) => string)

/** Options for {@link affectedDeployPlan}. */
export interface AffectedDeployPlanOptions extends ComputeAffectedOptions {
	/** How to assign the deploy version of each affected module. See {@link VersionPolicy}. */
	readonly version?: VersionPolicy
	/**
	 * Compute the served url for a module's new version. Defaults to keeping the
	 * module's current manifest url (suitable when urls are not version-stamped,
	 * or for plan previews); supply this to version-stamp the url.
	 */
	readonly urlFor?: (moduleId: string, version: string, previous: ModuleDeployment | undefined) => string
}

/**
 * One module the plan schedules to deploy: which module, the version to publish,
 * the url it will be served from, and the manifest entry it currently has (if
 * any). A module with no `previous` is new to the manifest (a first deploy).
 */
export interface PlannedDeploy {
	/** The module to deploy. */
	readonly moduleId: string
	/** The version to publish for it. */
	readonly version: string
	/** The url the new version will be served from. */
	readonly url: string
	/** The module's current manifest deployment, or `undefined` if it is new. */
	readonly previous: ModuleDeployment | undefined
}

/**
 * A single manifest delta the plan implies: the module, its prior deployment (or
 * `undefined` if newly added), and the deployment it will point at once the plan
 * is applied. The set of these is exactly the difference between the input
 * manifest and {@link AffectedDeployPlan.nextManifest}.
 */
export interface ManifestChange {
	/** The module whose manifest entry changes. */
	readonly moduleId: string
	/** Prior deployment, or `undefined` if the module is being added. */
	readonly from: ModuleDeployment | undefined
	/** Deployment the module will point at after the plan is applied. */
	readonly to: ModuleDeployment
}

/**
 * The output of {@link affectedDeployPlan}: the modules to compile, test, and
 * deploy (compile/test are the full affected set; deploy is the same set with
 * resolved version+url), the manifest deltas that result, and the resulting
 * manifest. When nothing is affected every array is empty and `nextManifest`
 * deep-equals the input — the signal for CI to do nothing.
 */
export interface AffectedDeployPlan {
	/** Affected modules to compile, sorted. */
	readonly compile: readonly string[]
	/** Affected modules to test, sorted (same set as {@link AffectedDeployPlan.compile}). */
	readonly test: readonly string[]
	/** Affected modules to deploy, with resolved version + url, sorted by `moduleId`. */
	readonly deploy: readonly PlannedDeploy[]
	/** The manifest deltas the plan implies, sorted by `moduleId`. */
	readonly manifestChanges: readonly ManifestChange[]
	/** The manifest after applying the plan (input manifest if nothing changed). */
	readonly nextManifest: FederationManifest
	/** `true` when no module is affected (CI can short-circuit). */
	readonly empty: boolean
}

/**
 * Plan a federated-deploy CI run from a change set.
 *
 * Computes the affected module set via {@link computeAffectedModules} (direct
 * edits + shared-lib fan-out), then for each affected module resolves the version
 * to deploy (per `options.version`) and the url it will serve from (per
 * `options.urlFor`), and produces the manifest deltas + resulting manifest. Pure:
 * it performs no I/O and never mutates the input manifest.
 *
 * The orchestrator's contract: compile + test every module in
 * {@link AffectedDeployPlan.compile}/`.test`, run a {@link DeployPlugin} for each
 * {@link PlannedDeploy} in `.deploy`, then publish `.nextManifest` (or apply
 * `.manifestChanges`). When `.empty`, skip the run.
 *
 * @param changedFiles The paths that changed.
 * @param graph The module dependency graph (see {@link ModuleDependencyGraph}).
 * @param manifest The current live manifest the plan diffs against.
 * @param options See {@link AffectedDeployPlanOptions}.
 */
export function affectedDeployPlan(
	changedFiles: Iterable<string>,
	graph: ModuleDependencyGraph,
	manifest: FederationManifest,
	options: AffectedDeployPlanOptions = {}
): AffectedDeployPlan {
	const affected = computeAffectedModules(changedFiles, graph, {
		...(options.matchFile ? { matchFile: options.matchFile } : {}),
		...(options.onMissingDependency ? { onMissingDependency: options.onMissingDependency } : {}),
	})

	const deploy: PlannedDeploy[] = []
	const manifestChanges: ManifestChange[] = []
	const nextModules: Record<string, ModuleDeployment> = { ...manifest.modules }

	for (const moduleId of affected) {
		const previous = manifest.modules[moduleId]
		const version = resolveVersion(options.version, moduleId, previous)
		const url = resolveUrl(options.urlFor, moduleId, version, previous)
		const to: ModuleDeployment = { version, url }

		deploy.push({ moduleId, version, url, previous })

		// Only a real change to the manifest entry counts as a delta.
		if (!previous || previous.version !== to.version || previous.url !== to.url) {
			manifestChanges.push({ moduleId, from: previous, to })
			nextModules[moduleId] = to
		}
	}

	const nextManifest: FederationManifest =
		manifestChanges.length === 0
			? manifest
			: freezeNext({
					schema: manifest.schema,
					...(manifest.app !== undefined ? { app: manifest.app } : {}),
					modules: nextModules,
					...(manifest.kinds ? { kinds: { ...manifest.kinds } } : {}),
				})

	return {
		compile: affected,
		test: affected,
		deploy,
		manifestChanges,
		nextManifest,
		empty: affected.length === 0,
	}
}

/** Resolve the deploy version for a module from the {@link VersionPolicy}. */
function resolveVersion(
	policy: VersionPolicy | undefined,
	moduleId: string,
	previous: ModuleDeployment | undefined
): string {
	if (typeof policy === 'function') {
		const v = policy(moduleId, previous)
		if (typeof v !== 'string' || v.length === 0) {
			throw new TypeError(`affectedDeployPlan: version policy returned an empty version for "${moduleId}"`)
		}
		return v
	}
	if (typeof policy === 'string') {
		if (policy.length === 0) {
			throw new TypeError('affectedDeployPlan: options.version must be a non-empty string')
		}
		return policy
	}
	// No policy: re-deploy the module's current version, or error if it is brand new.
	if (!previous) {
		throw new TypeError(
			`affectedDeployPlan: module "${moduleId}" is not in the manifest and no options.version was given to assign one`
		)
	}
	return previous.version
}

/** Resolve the served url for a module's new version (defaults to its current url). */
function resolveUrl(
	urlFor: AffectedDeployPlanOptions['urlFor'],
	moduleId: string,
	version: string,
	previous: ModuleDeployment | undefined
): string {
	if (urlFor) {
		const url = urlFor(moduleId, version, previous)
		if (typeof url !== 'string' || url.length === 0) {
			throw new TypeError(`affectedDeployPlan: urlFor returned an empty url for "${moduleId}"`)
		}
		return url
	}
	if (!previous) {
		throw new TypeError(
			`affectedDeployPlan: module "${moduleId}" is new; supply options.urlFor to assign its url`
		)
	}
	return previous.url
}

/** Freeze the assembled next manifest (mirrors the manifest module's invariant). */
function freezeNext(manifest: FederationManifest): FederationManifest {
	for (const dep of Object.values(manifest.modules)) Object.freeze(dep)
	Object.freeze(manifest.modules)
	if (manifest.kinds) Object.freeze(manifest.kinds)
	return Object.freeze(manifest)
}
