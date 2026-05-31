/**
 * @module
 *
 * `@treaty/deploy` — **build-to-deploy** for Treaty apps: the step between a
 * finished build and a live release.
 *
 * Treaty compiles every lazy feature route and library into its own independently
 * versioned, deployable, rollback-able federated module (see
 * `@treaty/module-federation`); `@treaty/federation-deploy` is the versioned
 * {@link FederationManifest} + pluggable deploy-method layer over that. This
 * package assembles the two into a release:
 *
 *   - {@link assembleDeployArtifact} — walk a built output directory against a
 *     manifest and produce a {@link DeployArtifact}: the exact files to upload,
 *     the manifest to publish, and the per-module **versioned** upload paths
 *     (`<moduleId>/<version>/<file>`) that make partial deploy and rollback
 *     possible. Pure path/byte planning — no upload.
 *   - {@link deploy} — hand that artifact to a pluggable {@link DeployTarget}
 *     (`upload`/`urlFor`), upload every file, resolve each module's served url,
 *     and return the manifest to publish. Treaty is a compiler, not a host: this
 *     emits the artifact and invokes a target; it never runs a server.
 *   - {@link FsDeployTarget} — the reference self-hosted target (writes to a local
 *     directory). A {@link CloudDeployTarget} is the same shape over an object
 *     store / static host / the Treaty cloud — "self-hosted and Treaty cloud" are
 *     pluggable.
 *   - {@link deployViaPlugin} — drive a release through any
 *     `@treaty/federation-deploy` {@link DeployPlugin} (the existing
 *     `FsDeployPlugin`/`NoopDeployPlugin` + registry) instead of a per-file target.
 *   - {@link rollback} — flip a single manifest entry to an already-published
 *     prior version, uploading nothing.
 *
 * **Partial deploy** is first-class: pass `only` or `changedFiles` + `graph` to
 * {@link assembleDeployArtifact} and the artifact (hence the deploy) covers only
 * the affected modules — the unit of independent deploy/rollback.
 *
 * Federation is **toggleable**: pass `options.federation` to {@link deploy} /
 * {@link deployViaPlugin} and a `false` / `{ enabled: false }` toggle makes the
 * deploy a no-op (nothing uploaded, manifest returned unchanged). And it is
 * **ejectable**: {@link manifestModulesFromConfig} (re-exported from
 * `@treaty/federation-deploy`) seeds a manifest straight from a user-owned,
 * ejected standalone `@module-federation/enhanced` config.
 */

export {
	assembleDeployArtifact,
	artifactFiles,
	artifactPaths,
} from './artifact.js'
export type {
	DeployArtifact,
	ModuleArtifact,
	AssembleDeployArtifactOptions,
} from './artifact.js'

export { deploy, deployViaPlugin, rollback } from './deploy.js'
export type {
	DeployTarget,
	CloudDeployTarget,
	DeployTargetContext,
	DeployOptions,
	DeployResult,
	ModuleDeployResult,
	DeployViaPluginOptions,
	RollbackOptions,
} from './deploy.js'

export { deployRemote, rollbackRemote, deployAffectedRemotes } from './remote.js'
export type {
	RemoteArtifact,
	AffectedRemoteArtifact,
	RemoteDeployDeps,
	DeployRemoteOptions,
	RollbackRemoteOptions,
	RemoteDeployResult,
	DeployAffectedRemotesOptions,
	DeployAffectedRemotesResult,
} from './remote.js'

export { FsDeployTarget, FsDeploymentStore } from './targets.js'
export type { FsDeployTargetOptions, FsDeploymentStoreOptions } from './targets.js'

// Re-exported from `@treaty/federation-deploy` so a deploy caller has the toggle
// reader and the ejected-config bridge available from one import: skip a deploy
// when federation is off, or seed a manifest from a user-owned ejected config.
export {
	isFederationEnabled,
	manifestModulesFromConfig,
	federatedModuleIdsFromConfig,
} from '@treaty/federation-deploy'
export type {
	FederationToggle,
	EjectedFederationConfig,
	ManifestModulesFromConfigOptions,
} from '@treaty/federation-deploy'

// The versioned deployment-manifest layer (per-remote ledger + history) and its
// store/runtime, re-exported so a deploy caller drives deployRemote/rollbackRemote
// and resolves remotes at runtime from one import.
export {
	createDeploymentManifest,
	getRemote,
	hasRemote,
	recordDeployment,
	rollbackTo,
	serializeDeploymentManifest,
	parseDeploymentManifest,
	MemoryDeploymentStore,
	createTreatyDeploymentRuntimePlugin,
	DEPLOYMENT_MANIFEST_SCHEMA,
} from '@treaty/federation-deploy'
export type {
	DeploymentManifest,
	RemoteDeployment,
	RemoteKind,
	DeploymentManifestStore,
	DeploymentManifestSource,
} from '@treaty/federation-deploy'
