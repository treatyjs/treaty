/**
 * @module
 *
 * The `ng generate @treaty/schematics:library` schematic. It scaffolds a Treaty
 * library that is exposable as a Module Federation **remote** out-of-the-box,
 * with zero MF configuration:
 *
 *   - the library is registered in `angular.json` against the `@treaty/build`
 *     builder;
 *   - a `federation.config.json` is written next to it that exposes the
 *     library's public API (`./Module`), so any host can
 *     `import('<lib>/Module')` the moment the library is built.
 *
 * The Angular runtime is shared as eager singletons automatically, so a
 * federated host and this library always agree on one copy of the framework.
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

import { federationConfig, serializeFederationConfig } from '../federation.js'
import {
	TREATY_BUILD_BUILDER,
	readWorkspace,
	writeWorkspace,
	type WorkspaceProject,
} from '../workspace.js'

/** Options accepted by the {@link library} schematic (see `schema.json`). */
export interface LibraryOptions {
	/** Name of the new library. */
	name: string
	/** Component selector prefix. Defaults to `lib`. */
	prefix?: string
}

/** The `library` generator entry point. */
export function library(options: LibraryOptions): Rule {
	return (tree: Tree, _context: SchematicContext): Rule => {
		const name = options.name
		if (!name) {
			throw new Error('The "name" option is required for the Treaty library schematic.')
		}
		const dashed = strings.dasherize(name)
		const prefix = options.prefix ?? 'lib'

		const workspace = readWorkspace(tree)
		const newProjectRoot = workspace.newProjectRoot ?? 'projects'
		const root = `${newProjectRoot}/${dashed}`
		const sourceRoot = `${root}/src`

		const project: WorkspaceProject = {
			projectType: 'library',
			root,
			sourceRoot,
			prefix,
			architect: {
				build: {
					builder: TREATY_BUILD_BUILDER,
					options: {
						tsConfig: `${root}/tsconfig.lib.json`,
					},
				},
			},
		}
		if (!workspace.projects) {
			workspace.projects = {}
		}
		workspace.projects[name] = project
		writeWorkspace(tree, workspace)

		// A library is a remote: it exposes its public entry point so any host can
		// consume it as `<lib>/Module` with zero MF configuration.
		const config = federationConfig({
			name: dashed,
			exposes: {
				'./Module': `./${sourceRoot}/public-api.ts`,
			},
		})

		const templateSource = apply(url('./files'), [
			applyTemplates({
				...strings,
				name,
				prefix,
				federationConfig: serializeFederationConfig(config),
			}),
			move(root),
		])

		return chain([mergeWith(templateSource)])
	}
}

export default library
