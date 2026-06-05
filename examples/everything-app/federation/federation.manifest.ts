/**
 * Federation deployment manifest for the everything-app.
 *
 * Federation is Treaty's unit of DEPLOYMENT GRANULARITY: the compiler emits the
 * host and every lazy route as an independently versioned, deployable,
 * rollback-able federated module. This file builds the manifest -- the runtime
 * ledger mapping every `moduleId` to its live `{ version, url }` -- with
 * `@treaty/federation-deploy`'s `buildManifest`, and shows how the platform
 * deploys or rolls back ONE module without rebuilding the rest.
 *
 * Treaty is a compiler, not a host: it emits the modules and this manifest; the
 * serving platform owns the manifest and flips it. The deploy/rollback helpers
 * are PURE -- each returns a NEW manifest with exactly one module repointed.
 */
import {
	buildManifest,
	serializeManifest,
	setModuleVersion,
	rollbackModule,
	createTreatyMfRuntimePlugin,
	type FederationManifest,
} from '@treaty/federation-deploy'

import { exposes } from './remote.config'

// One manifest entry per federated module: the host plus each auto-exposed lazy
// route. The route moduleIds line up with the `deriveExposesFromRoutes` keys on
// the remote (`./routes/dashboard`, `./routes/profile`), so the manifest and the
// federation config address the same modules.
export const manifest: FederationManifest = buildManifest(
	[
		{ moduleId: 'everything_app', version: '1.0.0', url: 'https://cdn.example.com/host/1.0.0/remoteEntry.js', kind: 'host' },
		...Object.keys(exposes).map((moduleId) => ({
			moduleId,
			version: '1.0.0',
			url: `https://cdn.example.com${moduleId.replace('./', '/')}/1.0.0/remoteEntry.js`,
			kind: 'route' as const,
		})),
	],
	{ app: 'everything-app' }
)

// The manifest is a committed deploy artifact; serialize it to canonical
// (sorted, diffable) JSON for the serving platform to publish.
export const manifestJson: string = serializeManifest(manifest)

/*
 * Deploy / rollback a SINGLE module -- no rebuild of the rest of the app.
 *
 * DEPLOY one route forward to v1.1.0 (every other module stays at v1.0.0):
 *
 *   const deployed = setModuleVersion(
 *     manifest,
 *     './routes/profile',
 *     '1.1.0',
 *     { url: 'https://cdn.example.com/routes/profile/1.1.0/remoteEntry.js' },
 *   )
 *
 * ROLLBACK that one route to the prior good version (again, nothing else moves):
 *
 *   const rolledBack = rollbackModule(
 *     deployed,
 *     './routes/profile',
 *     '1.0.0',
 *     { url: 'https://cdn.example.com/routes/profile/1.0.0/remoteEntry.js' },
 *   )
 *
 * The host registers the runtime plugin below; because it resolves each remote's
 * url+version from the manifest AT LOAD, flipping the manifest (deploy/rollback)
 * takes effect on the next load with no rebuild and no redeploy of the host.
 */

// Reference the deploy/rollback primitives so this example both type-checks them
// and demonstrates the exact call shape the platform uses.
export const deployedProfile: FederationManifest = setModuleVersion(
	manifest,
	'./routes/profile',
	'1.1.0',
	{ url: 'https://cdn.example.com/routes/profile/1.1.0/remoteEntry.js' }
)

export const rolledBackProfile: FederationManifest = rollbackModule(
	deployedProfile,
	'./routes/profile',
	'1.0.0',
	{ url: 'https://cdn.example.com/routes/profile/1.0.0/remoteEntry.js' }
)

// The runtime plugin the host registers (e.g. `init({ plugins: [runtimePlugin()] })`).
// It rewrites each remote's entry url+version from the manifest at load, so a
// manifest flip repoints the next load without a rebuild.
export const runtimePlugin = createTreatyMfRuntimePlugin(manifest)
