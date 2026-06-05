/**
 * @module
 *
 * The `ng generate @treaty/schematics:application` schematic. It scaffolds a
 * Treaty application that is part of the Module Federation structure
 * out-of-the-box, with zero MF configuration:
 *
 *   - the project is registered in `angular.json` against the `@treaty/build`
 *     builders (so `ng build`/`ng serve` run Treaty);
 *   - a `federation.config.json` is written next to it. A **host** (`host:true`,
 *     the default) gets a config ready to consume remotes; a **remote**
 *     (`host:false`) gets a config that exposes its root component and routes,
 *     so a host can `import('<app>/Component')` immediately.
 *
 * Either way the Angular runtime is shared as eager singletons automatically.
 */

import {
	apply,
	applyTemplates,
	chain,
	mergeWith,
	move,
	url,
	type Rule,
	type SchematicContext,
	type Tree,
} from '@angular-devkit/schematics'
import { strings } from '@angular-devkit/core'

import { defaultRemoteExposes, federationConfig, serializeFederationConfig } from '../federation.js'
import {
	TREATY_BUILD_BUILDER,
	TREATY_SERVE_BUILDER,
	readWorkspace,
	writeWorkspace,
	type WorkspaceProject,
} from '../workspace.js'

/** Options accepted by the {@link application} schematic (see `schema.json`). */
export interface ApplicationOptions {
	/** Name of the new application. */
	name: string
	/** Generate a federation host (true) or a pure remote (false). */
	host?: boolean
	/** Component selector prefix. Defaults to `app`. */
	prefix?: string
}

/** Build the per-project federation config text for a host or a remote. */
function federationFor(name: string, sourceRoot: string, isHost: boolean): string {
	const dashed = strings.dasherize(name)
	const config = isHost
		? federationConfig({ name: dashed })
		: federationConfig({ name: dashed, exposes: defaultRemoteExposes(`./${sourceRoot}`) })
	return serializeFederationConfig(config)
}

/** The `application` generator entry point. */
export function application(options: ApplicationOptions): Rule {
	return (tree: Tree, _context: SchematicContext): Rule => {
		const name = options.name
		if (!name) {
			throw new Error('The "name" option is required for the Treaty application schematic.')
		}
		const dashed = strings.dasherize(name)
		const isHost = options.host !== false
		const prefix = options.prefix ?? 'app'

		const workspace = readWorkspace(tree)
		const newProjectRoot = workspace.newProjectRoot ?? 'projects'
		const root = `${newProjectRoot}/${dashed}`
		const sourceRoot = `${root}/src`

		const project: WorkspaceProject = {
			projectType: 'application',
			root,
			sourceRoot,
			prefix,
			architect: {
				build: {
					builder: TREATY_BUILD_BUILDER,
					options: {
						outputPath: `dist/${dashed}`,
						index: `${sourceRoot}/index.html`,
						browser: `${sourceRoot}/main.ts`,
						tsConfig: `${root}/tsconfig.app.json`,
					},
				},
				serve: {
					builder: TREATY_SERVE_BUILDER,
					options: { port: isHost ? 4200 : 4201 },
				},
			},
		}
		if (!workspace.projects) {
			workspace.projects = {}
		}
		workspace.projects[name] = project
		writeWorkspace(tree, workspace)

		const templateSource = apply(url('./files'), [
			applyTemplates({
				...strings,
				name,
				prefix,
				host: isHost,
				federationConfig: federationFor(name, sourceRoot, isHost),
			}),
			move(root),
		])

		return chain([mergeWith(templateSource)])
	}
}

export default application
