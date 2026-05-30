/**
 * @module
 *
 * Shared helpers for the Treaty schematics. These read and mutate the Angular
 * workspace file (`angular.json`) inside a schematics {@link Tree}, treating it
 * as the single source of truth for how a project is built and served.
 *
 * The schematics never ask the developer to wire Module Federation or pick a
 * builder: this module encodes the Treaty defaults so `ng add`/`ng generate`
 * leave behind a workspace that is a federation host (and exposable remote)
 * automatically.
 */

import type { Tree } from '@angular-devkit/schematics'
import { SchematicsException } from '@angular-devkit/schematics'

/** The path of the Angular workspace file Treaty operates on. */
export const WORKSPACE_PATH = '/angular.json'

/**
 * The Treaty build builder. Mirrors the modern Angular builder naming
 * (`@angular/build:application`) so a project's `architect.build.builder`
 * can be swapped to Treaty with no other change. Runs the Rust Treaty
 * compiler + the bundler/MF plugins under the hood.
 */
export const TREATY_BUILD_BUILDER = '@treaty/build:application'

/** The Treaty dev-server builder, used for the project's `serve` target. */
export const TREATY_SERVE_BUILDER = '@treaty/build:dev-server'

/** The Treaty production extraction builder, used for the `extract-i18n` target. */
export const TREATY_EXTRACT_I18N_BUILDER = '@treaty/build:extract-i18n'

/**
 * The minimal shape of an `angular.json` we rely on. The real schema is far
 * larger; we only type the parts the Treaty schematics read or write so the
 * mutations are type-checked without dragging in the full workspace schema.
 */
export interface WorkspaceTarget {
	builder: string
	options?: Record<string, unknown>
	configurations?: Record<string, Record<string, unknown>>
	[key: string]: unknown
}

export interface WorkspaceProject {
	projectType?: 'application' | 'library'
	root?: string
	sourceRoot?: string
	prefix?: string
	architect?: Record<string, WorkspaceTarget>
	/** Angular >= 17 renamed `architect` to `targets`; we honour both. */
	targets?: Record<string, WorkspaceTarget>
	[key: string]: unknown
}

export interface WorkspaceSchema {
	version?: number
	newProjectRoot?: string
	defaultProject?: string
	projects?: Record<string, WorkspaceProject>
	[key: string]: unknown
}

/** Read and parse `angular.json` from the tree, throwing if it is missing. */
export function readWorkspace(tree: Tree): WorkspaceSchema {
	const buffer = tree.read(WORKSPACE_PATH)
	if (buffer === null) {
		throw new SchematicsException(
			`Could not find an Angular workspace at ${WORKSPACE_PATH}. ` +
				`Run \`ng add @treaty/schematics\` from the root of an Angular workspace.`,
		)
	}
	try {
		return JSON.parse(buffer.toString('utf-8')) as WorkspaceSchema
	} catch (err) {
		const message = err instanceof Error ? err.message : String(err)
		throw new SchematicsException(`Invalid JSON in ${WORKSPACE_PATH}: ${message}`)
	}
}

/** Serialize the workspace back into the tree (overwriting `angular.json`). */
export function writeWorkspace(tree: Tree, workspace: WorkspaceSchema): void {
	tree.overwrite(WORKSPACE_PATH, `${JSON.stringify(workspace, null, 2)}\n`)
}

/**
 * Return the target map for a project, accommodating both the legacy
 * `architect` key and the newer `targets` key. Mutating the returned object
 * mutates the project in place.
 */
export function targetsOf(project: WorkspaceProject): Record<string, WorkspaceTarget> {
	if (project.targets) {
		return project.targets
	}
	if (!project.architect) {
		project.architect = {}
	}
	return project.architect
}

/** Look up a project by name, throwing a helpful error when it is absent. */
export function getProject(workspace: WorkspaceSchema, name: string): WorkspaceProject {
	const project = workspace.projects?.[name]
	if (!project) {
		throw new SchematicsException(
			`Project "${name}" was not found in ${WORKSPACE_PATH}.`,
		)
	}
	return project
}

/**
 * Pick the project a schematic should default to: the workspace's
 * `defaultProject` if set, otherwise the sole application, otherwise the first
 * project. Returns `undefined` for an empty workspace.
 */
export function defaultProjectName(workspace: WorkspaceSchema): string | undefined {
	if (workspace.defaultProject && workspace.projects?.[workspace.defaultProject]) {
		return workspace.defaultProject
	}
	const projects = workspace.projects ?? {}
	const names = Object.keys(projects)
	const apps = names.filter((n) => projects[n]?.projectType !== 'library')
	if (apps.length === 1) {
		return apps[0]
	}
	return names[0]
}
