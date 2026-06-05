/**
 * @module
 *
 * Workspace COMPONENT REGISTRY: the selectorless / cross-file knowledge that
 * powers Treaty's template intelligence (completions, auto-import, hover and
 * go-to-definition on a `<panel/>` tag or a `use:highlight` directive).
 *
 * Treaty is *selectorless*: a lowercase template tag (`<panel/>`) or `use:`
 * directive name resolves to an imported (or same-folder) `@Component` /
 * `@Directive` by a filename/class-name fold, with no `NgModule` and no
 * `imports: [...]` array to enumerate. To beat the Angular Language Service on
 * this axis the server must therefore *discover* the project's components itself
 * and offer them — with an auto-import edit — wherever a tag can go.
 *
 * The registry is built by scanning the project's authoring sources:
 *
 *  - `.treaty` / `.tsx` / `.tjsx` — the filename is the component name; its
 *    selectorless tag is the kebab-case of the file stem (`StatCard.treaty` →
 *    `<stat-card>`), matching `rust_authoring`'s filename convention.
 *  - `.ts` — ordinary Angular `@Component` / `@Directive` classes, whose real
 *    `selector` string is read by the Rust scanner (`scanProjectSelectors`,
 *    NAPI `buildSelectorRegistry`). These contribute their declared selector
 *    (e.g. `app-stat-card`) so a parent resolves `<app-stat-card>` correctly.
 *
 * The compiler is never reimplemented here: `.ts` selectors come straight from
 * the Rust scanner, and the `.treaty`/JSX fold mirrors the documented filename
 * convention. The registry only *indexes* what the compiler would resolve.
 */

import { basename, extname } from 'node:path'
import {
	importedSelectorsFor,
	scanProjectSelectors,
	type ImportedSelectorMap,
	type ProjectSelectors,
} from './compiler.js'

/** What kind of declaration a registered entry is. */
export type ComponentKind = 'component' | 'directive'

/** Which authoring front-end a registered entry comes from. */
export type ComponentOrigin = 'treaty' | 'jsx' | 'angular' | 'builtin'

/** A single resolvable template dependency known to the workspace. */
export interface ComponentEntry {
	/** The class/binding name the consumer imports (e.g. `StatCard`). */
	readonly className: string
	/**
	 * The selectorless tag this entry renders as in a template, lowercase
	 * (e.g. `stat-card` or `app-stat-card`). Used to match a template `<tag>` and
	 * as the completion insert text. For an ATTRIBUTE-selector directive (whose
	 * selector is `[routerLink]`, not an element name) this is the kebab fold of
	 * the class — a fallback only; {@link attributeSelector} is the real handle.
	 */
	readonly tag: string
	/**
	 * The ATTRIBUTE-selector name a directive applies under, WITHOUT `use:` — the
	 * inner name of a `[name]` selector (`[routerLink]` → `routerLink`). Present
	 * only for attribute-selector directives (built-in Angular like `routerLink`,
	 * or any imported `@Directive({ selector: '[x]' })`); `undefined` for an
	 * element-selector component/directive. These complete in plain attribute
	 * position and auto-import — `use:` is optional/redundant for them.
	 */
	readonly attributeSelector?: string
	/** Component vs directive (directives surface under `use:` too). */
	readonly kind: ComponentKind
	/** Which authoring format declared it. */
	readonly origin: ComponentOrigin
	/** Absolute file path/URI string the declaration lives in (for go-to-def). */
	readonly fileName: string
	/**
	 * The module specifier a consumer would import this from, derived from
	 * {@link fileName} (extension dropped), or a bare package specifier for a
	 * built-in (`@angular/router`). Used to synthesize an auto-import.
	 */
	readonly importSpecifier: string
}

/**
 * One source file's contribution to the registry, before it is merged into the
 * project view. Kept per-file so an edited/closed document can be re-indexed in
 * isolation without rescanning the workspace.
 */
interface FileContribution {
	readonly fileName: string
	readonly entries: readonly ComponentEntry[]
}

/**
 * The selectorless component view of a workspace. Holds every discovered
 * component/directive indexed by tag and by class name, refreshable per file as
 * documents change, plus the Rust project-selector map used to resolve a file's
 * imported selectors for a registry-aware compile.
 */
export class ComponentRegistry {
	private readonly byTag = new Map<string, ComponentEntry>()
	private readonly byClass = new Map<string, ComponentEntry>()
	private readonly byAttribute = new Map<string, ComponentEntry>()
	private readonly contributions = new Map<string, FileContribution>()
	private projectSelectors: ProjectSelectors = {}

	constructor() {
		// Seed the built-in Angular attribute-selector directives (routerLink, …)
		// so a bare `routerLink` completes + auto-imports out of the box, the way
		// the SelectorRegistry resolves an attribute selector — no `use:` required.
		this.contributions.set(BUILTIN_CONTRIBUTION_KEY, {
			fileName: BUILTIN_CONTRIBUTION_KEY,
			entries: BUILTIN_ATTRIBUTE_DIRECTIVES,
		})
		this.reindex()
	}

	/**
	 * Replace the project-wide `className → selector` map (from the Rust `.ts`
	 * scanner) and re-fold every `.ts`-origin entry's tag from it. `.treaty`/JSX
	 * entries are unaffected (their tag is the filename fold, not a selector).
	 */
	setProjectSelectors(selectors: ProjectSelectors): void {
		this.projectSelectors = selectors
		this.reindex()
	}

	/** The current project-wide selector map (for a registry-aware compile). */
	getProjectSelectors(): ProjectSelectors {
		return this.projectSelectors
	}

	/**
	 * Resolve the per-file imported-selector map for a source, so a registry-aware
	 * compile sees the same cross-module selectors a bundler build would.
	 */
	importedSelectorsFor(source: string): ImportedSelectorMap | undefined {
		return importedSelectorsFor(source, this.projectSelectors)
	}

	/**
	 * Index (or re-index) one source file's declarations. Replaces any prior
	 * contribution from the same file. A `.ts` file's selectors are taken from the
	 * project-selector map (already scanned by the Rust scanner); `.treaty`/JSX
	 * files fold their tag from the filename.
	 */
	indexFile(fileName: string, source: string): void {
		const entries = extractEntries(fileName, source, this.projectSelectors)
		this.contributions.set(fileName, { fileName, entries })
		this.reindex()
	}

	/** Drop a file's contribution (e.g. when the document is deleted). */
	removeFile(fileName: string): void {
		if (this.contributions.delete(fileName)) {
			this.reindex()
		}
	}

	/** Look up an entry by its selectorless tag (lowercase). */
	getByTag(tag: string): ComponentEntry | undefined {
		return this.byTag.get(tag.toLowerCase())
	}

	/** Look up an entry by class/binding name. */
	getByClass(className: string): ComponentEntry | undefined {
		return this.byClass.get(className)
	}

	/** Every known entry, in tag order (stable for deterministic completion lists). */
	all(): ComponentEntry[] {
		return [...this.byTag.values()].sort((a, b) => a.tag.localeCompare(b.tag))
	}

	/** Every directive entry (the `use:` candidates). */
	directives(): ComponentEntry[] {
		return this.all().filter((e) => e.kind === 'directive')
	}

	/**
	 * Every ATTRIBUTE-selector directive (built-in Angular like `routerLink`, plus
	 * any imported `@Directive({ selector: '[x]' })`), in attribute-name order.
	 * These complete in plain attribute position and auto-import — `use:` is
	 * optional/redundant for them.
	 */
	attributeDirectives(): ComponentEntry[] {
		return [...this.byAttribute.values()].sort((a, b) =>
			a.attributeSelector!.localeCompare(b.attributeSelector!),
		)
	}

	/** Look up an attribute-selector directive by its bare attribute name (case-insensitive). */
	getByAttribute(attribute: string): ComponentEntry | undefined {
		return this.byAttribute.get(attribute.toLowerCase())
	}

	/** Rebuild the tag/class/attribute indexes from the current per-file contributions. */
	private reindex(): void {
		this.byTag.clear()
		this.byClass.clear()
		this.byAttribute.clear()
		for (const contribution of this.contributions.values()) {
			for (const original of contribution.entries) {
				// Re-fold a `.ts` entry's tag from the latest project selectors so a
				// selector edited elsewhere is reflected without re-reading the file.
				const entry = refoldEntry(original, this.projectSelectors)
				if (!this.byTag.has(entry.tag)) {
					this.byTag.set(entry.tag, entry)
				}
				if (!this.byClass.has(entry.className)) {
					this.byClass.set(entry.className, entry)
				}
				if (entry.attributeSelector) {
					const key = entry.attributeSelector.toLowerCase()
					if (!this.byAttribute.has(key)) {
						this.byAttribute.set(key, entry)
					}
				}
			}
		}
	}
}

/** Synthetic file key under which the built-in attribute-selector directives are registered. */
const BUILTIN_CONTRIBUTION_KEY = '<builtin>'

/**
 * The built-in Angular attribute-selector directives a Treaty author applies
 * with a BARE attribute (no `use:`), recognized by their attribute selector the
 * way the Rust SelectorRegistry resolves `[routerLink]`. Each carries the
 * package its class is imported from, so accepting the completion auto-imports
 * the directive with no `NgModule`. This is the common, high-value subset
 * (router + common structural/attribute directives); a project's own imported
 * attribute-selector directives are discovered from source on top of these.
 */
const BUILTIN_ATTRIBUTE_DIRECTIVES: readonly ComponentEntry[] = (
	[
		['RouterLink', 'routerLink', '@angular/router'],
		['RouterLinkActive', 'routerLinkActive', '@angular/router'],
		['NgClass', 'ngClass', '@angular/common'],
		['NgStyle', 'ngStyle', '@angular/common'],
		['NgModel', 'ngModel', '@angular/forms'],
	] as const
).map(([className, attributeSelector, importSpecifier]) => ({
	className,
	tag: tagFromClassName(className),
	attributeSelector,
	kind: 'directive' as ComponentKind,
	origin: 'builtin' as ComponentOrigin,
	fileName: importSpecifier,
	importSpecifier,
}))

/**
 * Scan a workspace folder once for its `.ts` component selectors via the Rust
 * scanner, returning the project-wide map. Best-effort: a missing folder yields
 * an empty map (no throw), matching the NAPI scanner's tolerance.
 */
export function scanWorkspaceSelectors(rootDir: string): ProjectSelectors {
	try {
		return scanProjectSelectors(rootDir)
	} catch {
		return {}
	}
}

/**
 * Extract the component/directive entries a single source file declares.
 *
 * `.treaty` / `.tsx` / `.tjsx` contribute exactly one entry whose tag is the
 * kebab-cased file stem (the filename convention). `.ts` files contribute every
 * `@Component`/`@Directive` class found in the project-selector map that this
 * file declares — matched by re-reading the class names the file exports — so a
 * conventional Angular component keeps its real `selector`.
 */
function extractEntries(
	fileName: string,
	source: string,
	projectSelectors: ProjectSelectors,
): ComponentEntry[] {
	const ext = extname(fileName).toLowerCase()
	const importSpecifier = stripExtension(fileName)

	if (ext === '.treaty' || ext === '.tsx' || ext === '.tjsx') {
		const className = classNameFromFileName(fileName)
		const origin: ComponentOrigin = ext === '.treaty' ? 'treaty' : 'jsx'
		// A `.treaty`/JSX file with a `host { … }` block (and no template) is a
		// directive; otherwise a component. The fold tag is identical either way.
		const kind: ComponentKind = isDirectiveSource(source) ? 'directive' : 'component'
		return [
			{
				className,
				tag: tagFromClassName(className),
				kind,
				origin,
				fileName,
				importSpecifier,
			},
		]
	}

	if (ext === '.ts') {
		return angularEntries(fileName, source, projectSelectors, importSpecifier)
	}

	return []
}

/**
 * Build entries for an Angular `.ts` source: every top-level `@Component` /
 * `@Directive` class declared in the file whose selector is known in the
 * project map (scanned by the Rust scanner). The class names come from a light
 * scan of the file's own `class <Name>` declarations decorated with
 * `@Component`/`@Directive`; the selector and kind come from the project map /
 * decorator so the compiler stays the source of truth for selectors.
 */
function angularEntries(
	fileName: string,
	source: string,
	projectSelectors: ProjectSelectors,
	importSpecifier: string,
): ComponentEntry[] {
	const entries: ComponentEntry[] = []
	for (const decl of scanAngularDeclarations(source)) {
		const selector = projectSelectors[decl.className]
		// Only surface a `.ts` class once the Rust scanner has resolved a real
		// string selector for it; a class with no static selector is not a
		// selectorless template dependency we can suggest.
		if (!selector) {
			continue
		}
		const attributeSelector = attributeSelectorOf(selector)
		entries.push({
			className: decl.className,
			tag: firstTagOfSelector(selector),
			...(attributeSelector ? { attributeSelector } : {}),
			kind: decl.kind,
			origin: 'angular',
			fileName,
			importSpecifier,
		})
	}
	return entries
}

/** Re-fold a `.ts` entry's tag/attribute-selector from the current project selectors (no-op otherwise). */
function refoldEntry(entry: ComponentEntry, projectSelectors: ProjectSelectors): ComponentEntry {
	if (entry.origin !== 'angular') {
		return entry
	}
	const selector = projectSelectors[entry.className]
	if (!selector) {
		return entry
	}
	const tag = firstTagOfSelector(selector)
	const attributeSelector = attributeSelectorOf(selector)
	if (tag === entry.tag && attributeSelector === entry.attributeSelector) {
		return entry
	}
	return {
		...entry,
		tag,
		...(attributeSelector ? { attributeSelector } : { attributeSelector: undefined }),
	}
}

/** A lightweight `@Component`/`@Directive class X` declaration found in a `.ts` source. */
interface AngularDeclaration {
	readonly className: string
	readonly kind: ComponentKind
}

/**
 * Scan a `.ts` source for top-level classes immediately preceded by an
 * `@Component(` or `@Directive(` decorator, recording the class name and kind.
 * Deliberately shallow (no full parse): the authoritative selector still comes
 * from the Rust scanner; this only enumerates which classes a file declares so
 * the right import specifier and kind are attached.
 */
function scanAngularDeclarations(source: string): AngularDeclaration[] {
	const out: AngularDeclaration[] = []
	const seen = new Set<string>()
	const re = /@(Component|Directive)\s*\([\s\S]*?\)\s*(?:export\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)/g
	for (let m = re.exec(source); m; m = re.exec(source)) {
		const kind: ComponentKind = m[1] === 'Directive' ? 'directive' : 'component'
		const className = m[2]!
		if (!seen.has(className)) {
			seen.add(className)
			out.push({ className, kind })
		}
	}
	return out
}

/**
 * Heuristic for whether a `.treaty`/JSX source declares a directive rather than
 * a component: a directive has a `host { … }` block (or `host:` map) and no
 * template/JSX-return view region. Mirrors the `.treaty` directive shape (no
 * view region + `host{}` block) documented for the authoring front-end.
 */
function isDirectiveSource(source: string): boolean {
	const hasHostBlock = /(^|\n)\s*host\s*\{/.test(source) || /\bhost\s*:/.test(source)
	if (!hasHostBlock) {
		return false
	}
	// A component has a template region: an HTML tag or a JSX `return <…>`.
	const hasView = /(^|\n)\s*</.test(source) || /return\s*\(?\s*</.test(source)
	return !hasView
}

/** The component class name implied by a `.treaty`/JSX filename (the file stem, PascalCased). */
export function classNameFromFileName(fileName: string): string {
	const stem = basename(fileName, extname(fileName))
	return pascalCase(stem)
}

/** Drop a path's extension, yielding the bare module specifier for an import. */
function stripExtension(fileName: string): string {
	const ext = extname(fileName)
	return ext ? fileName.slice(0, -ext.length) : fileName
}

/**
 * Fold a component class name to its selectorless template tag (kebab-case),
 * mirroring `rust_authoring`'s class↔tag fold: `StatCard` → `stat-card`.
 */
export function tagFromClassName(className: string): string {
	return className
		.replace(/([a-z0-9])([A-Z])/g, '$1-$2')
		.replace(/([A-Z]+)([A-Z][a-z])/g, '$1-$2')
		.toLowerCase()
}

/** The first type-selector of a CSS selector list (`app-card, .x` → `app-card`). */
function firstTagOfSelector(selector: string): string {
	const first = selector.split(',')[0]!.trim()
	// Strip attribute/class/pseudo qualifiers, keep the leading element name.
	const m = /^[A-Za-z][\w-]*/.exec(first)
	return (m ? m[0] : first).toLowerCase()
}

/**
 * The ATTRIBUTE-selector name of a directive selector, if any. Reads the FIRST
 * `[attr]` group across the selector list, returning its inner attribute name
 * preserving case (`[routerLink]` → `routerLink`, `a[routerLink]` →
 * `routerLink`, `[appHighlight]` → `appHighlight`). An attribute selector that
 * pins a value (`[type=text]`) is not a directive-application handle, so only a
 * bare `[name]` group counts. Returns `undefined` for a pure element selector.
 */
function attributeSelectorOf(selector: string): string | undefined {
	for (const part of selector.split(',')) {
		const m = /\[\s*([A-Za-z_][\w-]*)\s*\]/.exec(part)
		if (m) {
			return m[1]
		}
	}
	return undefined
}

/** PascalCase a file stem (`stat-card` / `stat_card` / `stat card` → `StatCard`). */
function pascalCase(stem: string): string {
	return stem
		.split(/[-_.\s]+/)
		.filter((part) => part.length > 0)
		.map((part) => part.charAt(0).toUpperCase() + part.slice(1))
		.join('')
}
