/**
 * @module
 *
 * Auto-provide of the shipped `@treaty/jsx` ambient JSX types.
 *
 * Treaty JSX (`.tsx` / `.tjsx` / `.treaty`) is selectorless, signal-aware and
 * standalone; the global `JSX` namespace that reflects that ships *from the
 * authoring plugin* (`@treaty/jsx`) so projects never hand-copy a
 * `treaty-jsx.d.ts`. The language server makes that promise hold in the editor
 * with no per-project `tsconfig` opt-in, in two complementary ways:
 *
 *  1. **Compiler-option auto-type.** The TypeScript project's
 *     {@link import('typescript').CompilerOptions compilerOptions} are augmented
 *     so `@treaty/jsx` is an auto-included ambient type (added to `types`) and
 *     the automatic JSX runtime resolves through it (`jsxImportSource` set to
 *     `@treaty/jsx`, with `jsx` lifted to `react-jsx` when unset). This is the
 *     idiomatic way TypeScript itself resolves the global `JSX` namespace, and
 *     is what {@link treatyJsxCompilerOptions} encodes.
 *  2. **Ambient root file.** The resolved ambient declaration `.d.ts` is also
 *     added as an extra project root file, so the global `JSX` namespace is
 *     loaded into the program even before `@treaty/jsx` resolves as a bare
 *     specifier (e.g. when the package is hoisted oddly in the workspace).
 *
 * Resolution is best-effort: when `@treaty/jsx` cannot be resolved (e.g. it is
 * not installed alongside the server), {@link resolveTreatyJsxTypesEntry}
 * returns `undefined`. The compiler-option auto-type still applies the bare
 * `@treaty/jsx` specifier (TypeScript resolves it from the open project), and
 * the root-file injection is simply skipped rather than failing — projects that
 * configure `"types"` / `jsxImportSource` themselves still work.
 */

import { createRequire } from 'node:module'
import { existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import type * as ts from 'typescript'

const require = createRequire(import.meta.url)

/**
 * Bare specifier of the shipped authoring-plugin types package. Used both as
 * the `jsxImportSource` and as the `types` entry that auto-includes the ambient
 * global `JSX` namespace.
 */
export const TREATY_JSX_IMPORT_SOURCE = '@treaty/jsx'

/**
 * Locate the shipped `@treaty/jsx` ambient declaration entry — the `.d.ts`
 * whose side-effecting imports install the global `JSX` namespace.
 *
 * Tries the package's resolved `types`/`main` first (the normal installed
 * case), then falls back to the sibling `libs/treaty/jsx/dist` build for the
 * in-repo / workspace layout. Returns `undefined` when nothing is found.
 */
export function resolveTreatyJsxTypesEntry(): string | undefined {
	const fromPackage = resolveFromPackage()
	if (fromPackage) {
		return fromPackage
	}
	return resolveFromSibling()
}

/** Resolve via the installed `@treaty/jsx` package's declaration entry. */
function resolveFromPackage(): string | undefined {
	try {
		// `require.resolve` yields the package's runtime entry (`dist/index.js`);
		// the co-located `index.d.ts` is the ambient declaration entry.
		const entry = require.resolve(TREATY_JSX_IMPORT_SOURCE)
		const declaration = entry.replace(/\.js$/, '.d.ts')
		return existsSync(declaration) ? declaration : undefined
	} catch {
		return undefined
	}
}

/** Resolve via the sibling `libs/treaty/jsx/dist/index.d.ts` workspace build. */
function resolveFromSibling(): string | undefined {
	let dir: string
	try {
		dir = dirname(fileURLToPath(import.meta.url))
	} catch {
		return undefined
	}
	// From `libs/treaty/lsp/{dist|src}` up to `libs/treaty`, then into `jsx/dist`.
	const candidate = join(dir, '..', '..', 'jsx', 'dist', 'index.d.ts')
	return existsSync(candidate) ? candidate : undefined
}

/**
 * Compute the `@treaty/jsx` auto-type augmentation for a TypeScript project's
 * compiler options.
 *
 * Given the project's current `options`, return the merged options that make a
 * Treaty `.tsx` / `.tjsx` / `.treaty` file resolve the global `JSX` namespace
 * with no hand-written `d.ts`:
 *
 *  - `types` gains `@treaty/jsx` (preserving any existing entries) so the
 *    ambient declarations are auto-included. When `types` is unset, TypeScript
 *    normally auto-includes *every* `@types` package; we preserve that default
 *    by leaving `types` unset *unless* the project already pinned a `types`
 *    list — in which case `@treaty/jsx` is appended so it is not dropped.
 *  - `jsxImportSource` is set to `@treaty/jsx` so the automatic runtime resolves
 *    element typing through `@treaty/jsx/jsx-runtime`.
 *  - `jsx` is lifted to `ReactJSX` (`"react-jsx"`) when the project left it
 *    unset, since `jsxImportSource` only takes effect under the automatic
 *    runtime. A project that already chose a `jsx` mode is left untouched.
 *
 * The returned object is a *new* object (the input is not mutated). The
 * function is pure and deterministic, which is what the language-plugin
 * configuration test asserts against.
 */
export function treatyJsxCompilerOptions(options: ts.CompilerOptions): ts.CompilerOptions {
	const merged: ts.CompilerOptions = { ...options }

	// Auto-include the ambient types. Only rewrite `types` when the project
	// already pinned it; otherwise leave it unset so TS keeps its default
	// auto-include behavior (and the ambient root file still loads the globals).
	if (Array.isArray(options.types)) {
		merged.types = options.types.includes(TREATY_JSX_IMPORT_SOURCE)
			? options.types
			: [...options.types, TREATY_JSX_IMPORT_SOURCE]
	}

	// Route the automatic JSX runtime through @treaty/jsx.
	merged.jsxImportSource = TREATY_JSX_IMPORT_SOURCE

	// jsxImportSource is only consulted under the automatic runtime; lift the
	// mode only when the project hasn't chosen one.
	if (merged.jsx === undefined) {
		// `4` is `ts.JsxEmit.ReactJSX`; encoded numerically to avoid importing the
		// TypeScript value module just for the enum.
		merged.jsx = 4 as ts.JsxEmit
	}

	return merged
}

/**
 * Minimal slice of the volarjs/TypeScript project host whose compiler options
 * and root-file list the auto-type augmentation wraps. Mirrors the relevant
 * members of `@volar/typescript`'s `TypeScriptProjectHost` without coupling the
 * wiring to that package's full type.
 */
export interface TreatyJsxProjectHost {
	getCompilationSettings(): ts.CompilerOptions
	getScriptFileNames(): string[]
}

/**
 * Apply the `@treaty/jsx` auto-type to `projectHost` so every file in the
 * project resolves the shipped global `JSX` namespace without per-project
 * configuration.
 *
 * Wraps {@link TreatyJsxProjectHost.getCompilationSettings} with
 * {@link treatyJsxCompilerOptions}, and — when `typesEntry` is provided — also
 * wraps {@link TreatyJsxProjectHost.getScriptFileNames} to add the ambient
 * declaration `.d.ts` as an extra project root file. Both wrappers are
 * idempotent: re-applying the augmentation does not duplicate the type or the
 * root file.
 */
export function applyTreatyJsxAutoTypes(
	projectHost: TreatyJsxProjectHost,
	typesEntry?: string,
): void {
	const originalSettings = projectHost.getCompilationSettings.bind(projectHost)
	projectHost.getCompilationSettings = () => treatyJsxCompilerOptions(originalSettings())

	if (typesEntry) {
		const originalFileNames = projectHost.getScriptFileNames.bind(projectHost)
		projectHost.getScriptFileNames = () => {
			const fileNames = originalFileNames()
			return fileNames.includes(typesEntry) ? fileNames : [...fileNames, typesEntry]
		}
	}
}
