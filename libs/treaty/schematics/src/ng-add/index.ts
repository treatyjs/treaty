/**
 * @module
 *
 * The `ng add @treaty/schematics` schematic. It converts an existing Angular
 * workspace to Treaty with zero developer configuration:
 *
 *   1. rewrites every (or the chosen) project's `build`/`serve`/`extract-i18n`
 *      targets in `angular.json` to the `@treaty/build` builders, so plain
 *      `ng build`/`ng serve` run the Treaty Rust compiler + the bundler/MF
 *      plugins;
 *   2. wires the chosen project as a Module Federation **host** by writing a
 *      `federation.config.json` next to it (Angular shared as eager singletons),
 *      and — unless skipped — scaffolds a sample **remote** application and
 *      registers it in the host's federation config.
 *
 * The developer is never asked to author a `ModuleFederationPlugin`: the host +
 * sample-remote structure falls out of `ng add` automatically.
 */

import {
	apply,
	applyTemplates,
	chain,
	mergeWith,
	move,
	noop,
	url,
	type Rule,
	type SchematicContext,
	type Tree,
} from '@angular-devkit/schematics'
import { strings } from '@angular-devkit/core'

import {
	FEDERATION_CONFIG_FILE,
	federationConfig,
	serializeFederationConfig,
} from '../federation.js'
import {
	TREATY_BUILD_BUILDER,
	TREATY_EXTRACT_I18N_BUILDER,
	TREATY_SERVE_BUILDER,
	defaultProjectName,
	getProject,
	readWorkspace,
	targetsOf,
	writeWorkspace,
	type WorkspaceProject,
} from '../workspace.js'

/** Options accepted by the {@link ngAdd} schematic (see `schema.json`). */
export interface NgAddOptions {
	/** Project to convert; defaults to the workspace default / sole app. */
	project?: string
	/** Wire the project as a federation host. Defaults to `true`. */
	host?: boolean
	/** Name of the sample remote to scaffold; empty/omitted with `skipRemote`. */
	remote?: string
	/** Skip scaffolding/wiring the sample remote (builders-only conversion). */
	skipRemote?: boolean
}

/** Map a project's `architect`/`targets` over to the Treaty build builders. */
function convertBuilders(project: WorkspaceProject): void {
	const targets = targetsOf(project)
	const build = targets['build']
	if (build) {
		build.builder = TREATY_BUILD_BUILDER
	}
	const serve = targets['serve']
	if (serve) {
		serve.builder = TREATY_SERVE_BUILDER
	}
	const extract = targets['extract-i18n']
	if (extract) {
		extract.builder = TREATY_EXTRACT_I18N_BUILDER
	}
}

/** Best-effort source root for a project (Angular omits it for `root: ''`). */
function projectSourceRoot(project: WorkspaceProject): string {
	if (project.sourceRoot) {
		return project.sourceRoot
	}
	const root = project.root ?? ''
	return root ? `${root}/src` : 'src'
}

/** Rewrite the builders for the chosen project in `angular.json`. */
function convertWorkspace(options: NgAddOptions): Rule {
	return (tree: Tree): Tree => {
		const workspace = readWorkspace(tree)
		const name = options.project ?? defaultProjectName(workspace)
		if (!name) {
			// Nothing to convert (empty workspace); leave the file untouched.
			return tree
		}
		const project = getProject(workspace, name)
		convertBuilders(project)
		writeWorkspace(tree, workspace)
		return tree
	}
}

/**
 * Write the host's `federation.config.json`, registering the sample remote (if
 * one is being scaffolded) so the host consumes it with zero MF config.
 */
function wireHostFederation(options: NgAddOptions): Rule {
	return (tree: Tree, context: SchematicContext): Tree => {
		if (options.host === false) {
			return tree
		}
		const workspace = readWorkspace(tree)
		const name = options.project ?? defaultProjectName(workspace)
		if (!name) {
			return tree
		}
		const project = getProject(workspace, name)
		const root = project.root ?? ''

		const wireRemote = !options.skipRemote && (options.remote ?? '') !== ''
		const remoteName = options.remote ?? 'remote'
		const remotes = wireRemote
			? {
					// A dev-time default entry; the Treaty dev-server serves the
					// scaffolded remote here and rewrites this at build time.
					[strings.dasherize(remoteName)]: `http://localhost:4201/remoteEntry.js`,
			  }
			: {}

		const config = federationConfig({ name: strings.dasherize(name), remotes })
		const configPath = root ? `/${root}/${FEDERATION_CONFIG_FILE}` : `/${FEDERATION_CONFIG_FILE}`
		const contents = serializeFederationConfig(config)
		if (tree.exists(configPath)) {
			tree.overwrite(configPath, contents)
		} else {
			tree.create(configPath, contents)
		}
		context.logger.info(
			`Treaty: ${name} is now a Module Federation host (${configPath}).`,
		)
		return tree
	}
}

/**
 * Scaffold a sample remote application, pre-wired as an exposable remote, so the
 * converted workspace demonstrates the host+remote federation structure with no
 * configuration. Delegates to the `application` generator with `host: false`.
 */
function scaffoldSampleRemote(options: NgAddOptions): Rule {
	if (options.skipRemote || (options.remote ?? '') === '') {
		return noop()
	}
	const remoteName = options.remote ?? 'remote'
	return (tree: Tree, context: SchematicContext): Rule => {
		const workspace = readWorkspace(tree)
		// Avoid clobbering an existing project of the same name.
		if (workspace.projects?.[remoteName]) {
			context.logger.info(
				`Treaty: project "${remoteName}" already exists; skipping sample remote scaffold.`,
			)
			return noop()
		}
		const newProjectRoot = workspace.newProjectRoot ?? 'projects'
		const root = `${newProjectRoot}/${strings.dasherize(remoteName)}`
		const sourceRoot = `${root}/src`

		// Register the project in angular.json, pre-wired to the Treaty builders.
		const project: WorkspaceProject = {
			projectType: 'application',
			root,
			sourceRoot,
			prefix: 'app',
			architect: {
				build: {
					builder: TREATY_BUILD_BUILDER,
					options: {
						outputPath: `dist/${strings.dasherize(remoteName)}`,
						index: `${sourceRoot}/index.html`,
						browser: `${sourceRoot}/main.ts`,
						tsConfig: `${root}/tsconfig.app.json`,
					},
				},
				serve: {
					builder: TREATY_SERVE_BUILDER,
					options: { port: 4201 },
				},
			},
		}
		if (!workspace.projects) {
			workspace.projects = {}
		}
		workspace.projects[remoteName] = project
		writeWorkspace(tree, workspace)

		const sourceTemplate = apply(url('../application/files'), [
			applyTemplates({
				...strings,
				name: remoteName,
				prefix: 'app',
				host: false,
				federationConfig: serializeFederationConfig(
					federationConfig({
						name: strings.dasherize(remoteName),
						exposes: {
							'./Component': `./${sourceRoot}/app/app.component.ts`,
							'./routes': `./${sourceRoot}/app/app.routes.ts`,
						},
					}),
				),
			}),
			move(root),
		])
		return mergeWith(sourceTemplate)
	}
}

/** The `ng add @treaty/schematics` entry point. */
export function ngAdd(options: NgAddOptions): Rule {
	return chain([
		convertWorkspace(options),
		scaffoldSampleRemote(options),
		wireHostFederation(options),
	])
}

export default ngAdd
