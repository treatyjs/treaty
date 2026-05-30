/**
 * @module
 *
 * Module Federation wiring for the Treaty schematics. Every Treaty project is a
 * federation participant automatically, so the schematics never ask the
 * developer to write a `ModuleFederationPlugin`: they call
 * {@link generateMfConfig} from `@treaty/module-federation` to produce the
 * normalized, Angular-singleton-sharing config and persist it next to the
 * project as `federation.config.json`.
 *
 * The Treaty build builders read that file; the developer edits it only if they
 * want to deviate from the zero-config default. A host's file lists the remotes
 * it consumes; a remote's file lists what it exposes. Both share the Angular
 * runtime as eager singletons by default.
 */

import { generateMfConfig } from '@treaty/module-federation'
import type { MfOptions, NormalizedMfConfig } from '@treaty/module-federation'

/** The filename Treaty uses for a project's persisted federation config. */
export const FEDERATION_CONFIG_FILE = 'federation.config.json'

/**
 * Build the normalized federation config for a project from simple options and
 * serialize it. Centralizing this here means a host and a remote produced by
 * different schematics agree on exactly one federation shape.
 */
export function federationConfig(options: MfOptions): NormalizedMfConfig {
	return generateMfConfig(options)
}

/** Serialize a federation config to the JSON text written into the tree. */
export function serializeFederationConfig(config: NormalizedMfConfig): string {
	return `${JSON.stringify(config, null, 2)}\n`
}

/**
 * The default expose map for a remote app: a remote exposes its root component
 * out-of-the-box so a host can `import('<remote>/Component')` with no further
 * configuration.
 */
export function defaultRemoteExposes(sourceRoot: string): Record<string, string> {
	return {
		'./Component': `${sourceRoot}/app/app.component.ts`,
		'./routes': `${sourceRoot}/app/app.routes.ts`,
	}
}
