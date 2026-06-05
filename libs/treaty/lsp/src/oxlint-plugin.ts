/**
 * @module
 *
 * Treaty's oxlint JS plugin: makes the linter understand the two ways a Treaty
 * authoring file (`.tsx` / `.tjsx` / `.treaty`) references an imported symbol
 * that the React-shaped `no-unused-vars` analysis cannot see, so authors never
 * need a `void X` statement to keep a real, used import:
 *
 *  1. **`use:<name>` directive application.** `use:highlight` applies the
 *     imported `Highlight` directive, but the attribute name is the lowercase
 *     directive name in the `use:` namespace, not a JS reference to `Highlight`.
 *  2. **Selectorless component tags.** `<panel />` renders the imported `panel`
 *     component, but a *lowercase* JSX tag is treated as an intrinsic DOM
 *     element by the standard analysis, so it never counts as a use of `panel`.
 *
 * Both forms are how Treaty's selectorless, case-insensitive front-end resolves
 * a template/`use:` name to an imported value; oxlint's built-in
 * `no-unused-vars` (a Rust rule) cannot be told a variable is used from a JS
 * plugin (oxc#20350), so this plugin instead *replaces* `no-unused-vars` for
 * Treaty files: the project disables the core rule for `.tsx` / `.tjsx` /
 * `.treaty` and enables `treaty/no-unused-vars`, which reports the exact same
 * unused bindings as the core rule **except** it additionally treats a binding
 * referenced via `use:<name>` or a selectorless lowercase tag as used.
 *
 * The rule mirrors the core `no-unused-vars` semantics: it walks the module
 * scope and every nested scope, skips exported declarations, skips `_`-prefixed
 * names, counts a binding used when it has any read reference (which already
 * covers ordinary identifier uses, type references, and capitalized JSX tags),
 * and reports imports, top-level declarations, locals, and parameters that are
 * otherwise unused. It carries no project/runtime coupling — it operates purely
 * on the AST and scope manager oxlint provides — so it is equally usable from a
 * repo `.oxlintrc.json` (`jsPlugins`) and from tests.
 */

/**
 * Minimal structural shapes of the oxlint JS-plugin API surface this rule
 * touches. Declared locally (rather than importing oxlint's alpha
 * `plugins-dev` types) so the plugin type-checks under the package's own
 * toolchain without depending on oxlint's unstable declaration entry.
 */

/** A node with at least a `type`; the AST is walked structurally. */
interface AstNode {
	readonly type: string
	readonly parent?: AstNode | null
	readonly [key: string]: unknown
}

/** A resolved reference to a variable, as exposed by the scope manager. */
interface Reference {
	isRead(): boolean
}

/** A binding definition (import, declaration, parameter, …). */
interface Definition {
	readonly type: string
	/** The binding identifier node (used as the diagnostic anchor). */
	readonly name: AstNode
	/** The declaration node the binding belongs to. */
	readonly node: AstNode
}

/** A variable in a scope: its name, references, and definitions. */
interface Variable {
	readonly name: string
	readonly references: readonly Reference[]
	readonly defs: readonly Definition[]
}

/** A lexical scope: its kind, its own variables, and nested scopes. */
interface Scope {
	readonly type: string
	readonly variables: readonly Variable[]
	readonly childScopes: readonly Scope[]
}

/** The scope manager for the linted file. */
interface ScopeManager {
	readonly scopes: readonly Scope[]
}

/** The source-code view oxlint passes to a rule. */
interface SourceCode {
	readonly ast: AstNode
	readonly scopeManager: ScopeManager
}

/** A diagnostic reported by the rule. */
interface ReportDescriptor {
	readonly node: AstNode
	readonly message: string
}

/** The rule context oxlint passes to `create`. */
interface RuleContext {
	readonly sourceCode: SourceCode
	report(descriptor: ReportDescriptor): void
}

/** A visitor object: AST-node-type → handler. */
type Visitor = Record<string, ((node: AstNode) => void) | undefined>

/** A single oxlint/ESLint-compatible rule. */
interface Rule {
	readonly meta?: { readonly name?: string; readonly type?: string }
	create(context: RuleContext): Visitor
}

/** An oxlint/ESLint-compatible plugin: a named bag of rules. */
export interface OxlintPlugin {
	readonly meta: { readonly name: string }
	readonly rules: Record<string, Rule>
}

/** The plugin id, used as the `<plugin>/<rule>` namespace in configs. */
export const TREATY_OXLINT_PLUGIN_NAME = 'treaty'

/** Fully-qualified id of the unused-vars rule this plugin contributes. */
export const TREATY_NO_UNUSED_VARS_RULE = `${TREATY_OXLINT_PLUGIN_NAME}/no-unused-vars`

/**
 * Definition kinds that name a declaration that may be exported (so an export
 * keeps the binding used). Excludes `Parameter` / `CatchClause`, whose `node`
 * is the enclosing function/clause and must not inherit that node's export.
 */
const EXPORTABLE_DEFINITION_TYPES = new Set([
	'ImportBinding',
	'Variable',
	'FunctionName',
	'ClassName',
	// TypeScript type-space declarations (type alias, interface, enum) — the
	// scope manager labels these `Type` / `TSEnumName`.
	'Type',
	'TSEnumName',
])

/** Export-declaration node types that keep a wrapped binding "used". */
const EXPORT_DECLARATION_TYPES = new Set([
	'ExportNamedDeclaration',
	'ExportDefaultDeclaration',
	'ExportAllDeclaration',
])

/** Declaration-node wrappers to climb through when checking for an export. */
const DECLARATION_WRAPPER_TYPES = new Set(['VariableDeclaration', 'VariableDeclarator'])

/**
 * Collect the two Treaty-specific reference forms from the whole AST:
 *
 *  - `useNames` — every `use:<name>` directive name, lower-cased, so a binding
 *    whose name matches (case-insensitively) is treated as applied.
 *  - `lowerTags` — every *lowercase-initial* JSX opening-element tag name, so a
 *    selectorless component tag counts as a use of the same-named binding.
 *    (Capitalized tags already produce ordinary identifier references, so they
 *    need no special handling.)
 */
function collectTreatyReferences(root: AstNode): {
	useNames: Set<string>
	lowerTags: Set<string>
} {
	const useNames = new Set<string>()
	const lowerTags = new Set<string>()
	const stack: unknown[] = [root]
	while (stack.length > 0) {
		const node = stack.pop()
		if (node === null || typeof node !== 'object') {
			continue
		}
		if (Array.isArray(node)) {
			for (const child of node) {
				stack.push(child)
			}
			continue
		}
		const record = node as Record<string, unknown>
		const type = record['type']
		if (type === 'JSXNamespacedName') {
			const namespace = record['namespace'] as Record<string, unknown> | undefined
			const name = record['name'] as Record<string, unknown> | undefined
			if (namespace && namespace['name'] === 'use' && name && typeof name['name'] === 'string') {
				useNames.add((name['name'] as string).toLowerCase())
			}
		} else if (type === 'JSXOpeningElement') {
			const name = record['name'] as Record<string, unknown> | undefined
			if (name && name['type'] === 'JSXIdentifier' && typeof name['name'] === 'string') {
				const tag = name['name']
				if (tag.length > 0 && isLowerInitial(tag)) {
					lowerTags.add(tag)
				}
			}
		}
		for (const key of Object.keys(record)) {
			if (key === 'parent') {
				continue
			}
			const value = record[key]
			if (value !== null && typeof value === 'object') {
				stack.push(value)
			}
		}
	}
	return { useNames, lowerTags }
}

/** Whether a tag/identifier starts with a lowercase letter (selectorless form). */
function isLowerInitial(name: string): boolean {
	const first = name[0]!
	return first === first.toLowerCase() && first !== first.toUpperCase()
}

/**
 * Whether a binding's declaration is directly exported (so it is "used" by the
 * module's public surface). Only declaration-kind defs can be exported; a
 * parameter's `node` is its function, whose own export must not be attributed
 * to the parameter, so parameter/catch defs are never treated as exported.
 */
function isExportedDeclaration(def: Definition): boolean {
	if (!EXPORTABLE_DEFINITION_TYPES.has(def.type)) {
		return false
	}
	let node: AstNode | null | undefined = def.node
	let parent = node?.parent
	// Climb only through the declaration's own wrappers
	// (VariableDeclarator → VariableDeclaration → Export…).
	let guard = 0
	while (parent && guard < 4) {
		if (EXPORT_DECLARATION_TYPES.has(parent.type)) {
			return true
		}
		if (DECLARATION_WRAPPER_TYPES.has(parent.type)) {
			node = parent
			parent = node.parent
			guard += 1
			continue
		}
		break
	}
	return false
}

/** Whether an import binding was brought in type-only (`import type` / `{ type X }`). */
function isTypeOnlyImport(def: Definition): boolean {
	const node = def.node
	if (node['importKind'] === 'type') {
		return true
	}
	const parent = node.parent
	return !!parent && parent['importKind'] === 'type'
}

/**
 * Build the diagnostic message for an unused binding, matching the wording
 * shape of the core `no-unused-vars` rule across the declaration kinds it
 * reports (import / type-import / variable / parameter / function / class /
 * type alias / interface / enum), including the `_` remediation hint the core
 * rule emits for locals and parameters.
 */
function unusedMessage(def: Definition, name: string): string {
	switch (def.type) {
		case 'Parameter':
			return `Parameter '${name}' is declared but never used. Unused parameters should start with a '_'.`
		case 'ImportBinding':
			return isTypeOnlyImport(def)
				? `Type '${name}' is imported but never used.`
				: `Identifier '${name}' is imported but never used.`
		case 'FunctionName':
			return `Function '${name}' is declared but never used.`
		case 'ClassName':
			return `Class '${name}' is declared but never used.`
		case 'TSEnumName':
			return `Enum '${name}' is declared but never used.`
		case 'Type':
			return def.node.type === 'TSInterfaceDeclaration'
				? `Interface '${name}' is declared but never used.`
				: `Type alias '${name}' is declared but never used.`
		default:
			return `Variable '${name}' is declared but never used. Unused variables should start with a '_'.`
	}
}

/**
 * The `treaty/no-unused-vars` rule. A Treaty-aware drop-in for the core
 * `no-unused-vars`: identical unused-binding reporting, plus it treats a
 * binding referenced via `use:<name>` or a selectorless lowercase tag as used.
 */
const noUnusedVars: Rule = {
	meta: {
		name: 'no-unused-vars',
		type: 'problem',
	},
	create(context: RuleContext): Visitor {
		const sourceCode = context.sourceCode
		return {
			'Program:exit'(program: AstNode): void {
				const { useNames, lowerTags } = collectTreatyReferences(program)
				// User declarations live in the module scope for an ES module, or
				// the global scope for a script (a file with no import/export). The
				// global scope also holds the built-in globals, but those carry no
				// definition (`defs` is empty), so the per-variable `def` guard
				// below filters them out.
				const scopes = sourceCode.scopeManager.scopes
				const rootScope =
					scopes.find((scope) => scope.type === 'module') ??
					scopes.find((scope) => scope.type === 'global')
				if (!rootScope) {
					return
				}

				const visited = new Set<Variable>()
				const queue: Scope[] = [rootScope]
				while (queue.length > 0) {
					const scope = queue.pop()!
					for (const variable of scope.variables) {
						if (!visited.has(variable)) {
							visited.add(variable)
							checkVariable(variable)
						}
					}
					for (const child of scope.childScopes) {
						queue.push(child)
					}
				}

				function checkVariable(variable: Variable): void {
					const def = variable.defs[0]
					if (!def) {
						return
					}
					// Enum members are not reported by the core rule; the enum itself
					// (its `TSEnumName` binding) carries the unused diagnostic.
					if (def.type === 'TSEnumMemberName') {
						return
					}
					// Ambient / overload-signature functions have no body: neither the
					// function name nor its parameters are reported by the core rule
					// (`declare function f(event)` / overload signatures).
					if (def.node.type === 'TSDeclareFunction') {
						return
					}
					const name = variable.name
					// `arguments` is an implicit binding the core rule never reports.
					if (name === 'arguments') {
						return
					}
					// The `_` prefix is the documented opt-out the core rule honors.
					if (name.startsWith('_')) {
						return
					}
					if (isExportedDeclaration(def)) {
						return
					}
					// Any read reference (ordinary use, type reference, or a
					// capitalized JSX tag) marks the binding used.
					if (variable.references.some((reference) => reference.isRead())) {
						return
					}
					// Treaty selectorless reference forms the standard analysis misses.
					if (useNames.has(name.toLowerCase())) {
						return
					}
					if (lowerTags.has(name)) {
						return
					}
					context.report({ node: def.name, message: unusedMessage(def, name) })
				}
			},
		}
	},
}

/**
 * The Treaty oxlint plugin. Wire it from a project's `.oxlintrc.json`:
 *
 * ```json
 * {
 *   "jsPlugins": ["@treaty/lsp/oxlint-plugin"],
 *   "overrides": [
 *     {
 *       "files": ["**\/*.tsx", "**\/*.tjsx", "**\/*.treaty"],
 *       "rules": {
 *         "no-unused-vars": "off",
 *         "treaty/no-unused-vars": "warn"
 *       }
 *     }
 *   ]
 * }
 * ```
 */
const treatyOxlintPlugin: OxlintPlugin = {
	meta: { name: TREATY_OXLINT_PLUGIN_NAME },
	rules: {
		'no-unused-vars': noUnusedVars,
	},
}

export default treatyOxlintPlugin
