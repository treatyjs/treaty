/**
 * @module
 *
 * `@treaty/schematics` — zero-config Angular schematics for Treaty. Treaty is a
 * compiler, not a host: these schematics arrange an Angular workspace so that
 * the developer configures nothing and every app is a Module Federation host
 * (and exposable remote) automatically.
 *
 *   - {@link ngAdd} (`ng add @treaty/schematics`) rewrites `angular.json` to the
 *     `@treaty/build` builders and scaffolds the host + sample-remote MF
 *     structure with shared Angular singletons.
 *   - {@link application} (`ng generate @treaty/schematics:application`) creates
 *     an app that is a federation host by default and exposable as a remote.
 *   - {@link library} (`ng generate @treaty/schematics:library`) creates a
 *     library exposable as a remote out-of-the-box.
 *
 * The schematic factories are wired through `collection.json` (referenced by the
 * package's `"schematics"` field); these re-exports let the factories be
 * consumed programmatically (and by the smoke test) as well.
 */

export { ngAdd } from './ng-add/index.js'
export type { NgAddOptions } from './ng-add/index.js'

export { application } from './application/index.js'
export type { ApplicationOptions } from './application/index.js'

export { library } from './library/index.js'
export type { LibraryOptions } from './library/index.js'

export {
	TREATY_BUILD_BUILDER,
	TREATY_SERVE_BUILDER,
	TREATY_EXTRACT_I18N_BUILDER,
	WORKSPACE_PATH,
} from './workspace.js'
export type {
	WorkspaceSchema,
	WorkspaceProject,
	WorkspaceTarget,
} from './workspace.js'

export {
	FEDERATION_CONFIG_FILE,
	federationConfig,
	serializeFederationConfig,
} from './federation.js'
