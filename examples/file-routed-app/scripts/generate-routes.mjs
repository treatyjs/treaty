// Build-time file-system route generation for the file-routed-app.
//
// Treaty's `treaty_file_routing` crate (libs/file-routing) is the canonical
// engine: it scans a `routes/` tree and lowers it to Angular lazy routes +
// Module Federation remotes. That engine is Rust with no JS binding, so this
// script is the JS surface that wires the SAME convention into a real bundler
// build: it scans `routes/` exactly as the crate's default `FileRoutingConfig`
// does and emits `src/generated/routes.ts` — an Angular `Routes` array of lazy
// `loadComponent` / `loadChildren` boundaries — plus the federation remote map.
//
// The conventions implemented here mirror libs/file-routing 1:1 for this tree
// (asserted against the crate's verified output in the app README):
//   - `index` / `page`            -> the directory's own route ("")
//   - `layout`                    -> a parent route wrapping its dir as children
//   - `not-found`                 -> a "**" wildcard route
//   - `[seg]` / `:seg`            -> dynamic segment, canonical Bracket spelling
//   - `[...seg]`                  -> catch-all dir lowers to a "[...seg]" segment
//   - `(group)`                   -> route group: paren name STRIPPED from URL,
//                                    children flattened with an empty segment
//   - a dir WITHOUT a layout      -> flattened: its segment is prefixed onto each
//                                    child path instead of nesting
//   - federation remote names     -> base slug from the (group-stripped) path,
//                                    de-duplicated deterministically in emission
//                                    order with a file-derived hint
//
// Run: node scripts/generate-routes.mjs   (also invoked by vite.config.ts)
import { readdirSync, statSync, writeFileSync, mkdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join, relative } from 'node:path'

const here = dirname(fileURLToPath(import.meta.url))
const appRoot = join(here, '..')
const routesDir = join(appRoot, 'routes')
const outFile = join(appRoot, 'src', 'generated', 'routes.ts')

const INDEX_NAMES = new Set(['index', 'page'])
const LAYOUT_NAME = 'layout'
const NOT_FOUND_NAME = 'not-found'
const ROUTE_EXTENSIONS = ['.treaty', '.tjsx', '.tsx', '.ts']

/** Split a file name into `{ base, ext }` using the recognised route extensions. */
function splitRouteFile(name) {
	for (const ext of ROUTE_EXTENSIONS) {
		if (name.endsWith(ext)) return { base: name.slice(0, -ext.length), ext }
	}
	return null
}

/** A directory's base name is a route group when wrapped in parentheses. */
function isRouteGroup(seg) {
	return seg.startsWith('(') && seg.endsWith(')')
}

/**
 * Normalise a directory segment to its URL form (canonical Bracket spelling):
 *   - `(group)`   -> ""        (no URL contribution)
 *   - `[...rest]` -> "[...rest]"
 *   - `[id]` / `:id` -> "[id]"
 *   - static name -> the name verbatim
 */
function normalizeSegment(seg) {
	if (isRouteGroup(seg)) return ''
	if (seg.startsWith('[...') && seg.endsWith(']')) return seg
	if (seg.startsWith(':...')) return `[...${seg.slice(4)}]`
	if (seg.startsWith('[') && seg.endsWith(']')) return seg
	if (seg.startsWith(':')) return `[${seg.slice(1)}]`
	return seg
}

/** Join two URL path fragments, dropping empties so `""` segments collapse. */
function joinPath(prefix, seg) {
	const parts = [prefix, seg].filter((p) => p !== '')
	return parts.join('/')
}

/** Slugify a (group-stripped) route path into a base federation remote name. */
function slugifyPath(path) {
	if (path === '') return 'root'
	if (path === '**') return 'not-found'
	return path
		.split('/')
		.map((s) => s.replace(/^\[\.\.\.(.+)\]$/, '$1').replace(/^\[(.+)\]$/, '$1'))
		.join('-')
		.toLowerCase()
}

/** Slugify a single path segment for a remote-name hint (group parens stripped). */
function slugifySegment(seg) {
	return seg
		.replace(/^\((.+)\)$/, '$1')
		.replace(/^\[\.\.\.(.+)\]$/, '$1')
		.replace(/^\[(.+)\]$/, '$1')
		.toLowerCase()
}

/** A file-derived disambiguation hint: containing dir for an index, else stem. */
function fileHint(entryFile) {
	const parts = entryFile.split('/')
	const fileName = parts[parts.length - 1]
	const { base } = splitRouteFile(fileName) ?? { base: fileName }
	if (INDEX_NAMES.has(base) && parts.length >= 2) return slugifySegment(parts[parts.length - 2])
	return slugifySegment(base)
}

/** Read a directory, classifying entries into subdirs and route files. */
function scanDir(absDir) {
	const dirs = []
	const files = []
	for (const name of readdirSync(absDir).sort()) {
		const abs = join(absDir, name)
		if (statSync(abs).isDirectory()) {
			dirs.push(name)
		} else {
			const split = splitRouteFile(name)
			if (split) files.push({ name, ...split })
		}
	}
	return { dirs, files }
}

/**
 * Lower one directory node into a list of Angular routes. `urlPrefix` is the URL
 * path accumulated from flattened ancestors; `treePrefix` is the tree-relative
 * directory path (for the entry-file references the loaders point at).
 */
function lowerNode(absDir, segment, urlPrefix, treePrefix) {
	const { dirs, files } = scanDir(absDir)
	const dirUrl = joinPath(urlPrefix, normalizeSegment(segment))

	const layout = files.find((f) => f.base === LAYOUT_NAME)
	const index = files.find((f) => INDEX_NAMES.has(f.base))
	const notFound = files.find((f) => f.base === NOT_FOUND_NAME)
	const pages = files.filter(
		(f) => !INDEX_NAMES.has(f.base) && f.base !== LAYOUT_NAME && f.base !== NOT_FOUND_NAME
	)

	/** Build a leaf route for a route file relative to the given base URL. */
	const leaf = (file, basePath) => {
		const isIndex = INDEX_NAMES.has(file.base)
		const path = isIndex ? basePath : joinPath(basePath, normalizeSegment(file.base))
		const entryFile = treePrefix === '' ? file.name : `${treePrefix}/${file.name}`
		return { path, component: entryFile, children: [], remoteBase: slugifyPath(path), entryFile }
	}

	// Recurse into subdirectories; each returns the routes it contributes.
	const childRoutesFor = (basePath, baseTree) => {
		const out = []
		// Index first (its own "" route), then pages, then child dirs, then 404 —
		// the scanner sorts lexicographically and emits in this documented order.
		if (index) out.push(leaf(index, basePath))
		for (const page of pages) out.push(leaf(page, basePath))
		for (const sub of dirs) {
			const subAbs = join(absDir, sub)
			out.push(...lowerNode(subAbs, sub, basePath, baseTree === '' ? sub : `${baseTree}/${sub}`))
		}
		if (notFound) {
			const entryFile = treePrefix === '' ? notFound.name : `${treePrefix}/${notFound.name}`
			out.push({ path: '**', component: entryFile, children: [], remoteBase: 'not-found', entryFile, wildcard: true })
		}
		return out
	}

	if (layout) {
		// Layout dir -> one parent route at this dir's URL, children relative to it.
		const layoutEntry = treePrefix === '' ? layout.name : `${treePrefix}/${layout.name}`
		return [
			{
				path: dirUrl,
				layout: layoutEntry,
				children: childRoutesFor('', treePrefix),
				remoteBase: slugifyPath(dirUrl),
				entryFile: layoutEntry,
			},
		]
	}

	// No layout -> flatten: contribute children with this dir's segment prefixed.
	return childRoutesFor(dirUrl, treePrefix)
}

/** Walk routes in emission order assigning globally-unique federation names. */
function assignRemoteNames(routes, used) {
	for (const route of routes) {
		const base = route.remoteBase
		if (!used.has(base)) {
			used.set(base, 1)
			route.remoteName = base
		} else {
			const hint = fileHint(route.entryFile)
			let candidate = `${base}-${hint}`
			let n = 2
			while (used.has(candidate)) candidate = `${base}-${hint}-${n++}`
			used.set(candidate, 1)
			route.remoteName = candidate
		}
		assignRemoteNames(route.children, used)
	}
}

/** Emit a route node as a TypeScript `Route` object literal. */
function emitRoute(route, indent) {
	const pad = '\t'.repeat(indent)
	const pad2 = '\t'.repeat(indent + 1)
	const lines = [`${pad}{`]
	lines.push(`${pad2}path: ${JSON.stringify(route.path)},`)
	const loaderTarget = route.layout ?? route.component
	if (route.children.length > 0) {
		// A parent (layout) route: load the layout component AND nest children.
		if (loaderTarget) {
			lines.push(
				`${pad2}loadComponent: () => import(${JSON.stringify('../../routes/' + loaderTarget)}),`
			)
		}
		lines.push(`${pad2}children: [`)
		for (const child of route.children) lines.push(emitRoute(child, indent + 2))
		lines.push(`${pad2}],`)
	} else {
		lines.push(
			`${pad2}loadComponent: () => import(${JSON.stringify('../../routes/' + loaderTarget)}),`
		)
	}
	lines.push(`${pad}},`)
	return lines.join('\n')
}

/** Collect every federation remote in emission order for the manifest export. */
function collectRemotes(routes, out) {
	for (const route of routes) {
		out.push({
			name: route.remoteName,
			exposedModule: './Route',
			entryFile: `routes/${route.entryFile}`,
			routePath: route.path,
		})
		collectRemotes(route.children, out)
	}
	return out
}

// --- run ---
const tree = lowerNode(routesDir, '', '', '')
assignRemoteNames(tree, new Map())
const remotes = collectRemotes(tree, [])

const banner = `// GENERATED by scripts/generate-routes.mjs — DO NOT EDIT.
//
// File-system routes lowered from \`routes/\` using the same convention the
// \`treaty_file_routing\` crate implements (see the app README for the verified
// expected output). Regenerate with \`node scripts/generate-routes.mjs\`; the
// Treaty Vite build runs it automatically in \`buildStart\`.
import type { Routes } from '@angular/router'
`

const routesSrc = `${banner}
/** The Angular route graph generated from the \`routes/\` directory tree. */
export const routes: Routes = [
${tree.map((r) => emitRoute(r, 1)).join('\n')}
]

export default routes

/**
 * Module Federation remotes derived from the lazy route + layout boundaries.
 * Every lazy route and layout boundary is one remote (\`exposedModule: './Route'\`);
 * names are globally unique, de-duplicated in emission order.
 */
export const federationRemotes = ${JSON.stringify(remotes, null, '\t')} as const
`

mkdirSync(dirname(outFile), { recursive: true })
writeFileSync(outFile, routesSrc)

console.log(
	`generate-routes: wrote ${relative(appRoot, outFile)} — ${remotes.length} remotes:\n` +
		remotes.map((r) => `  ${r.name}  path=${JSON.stringify(r.routePath)}  <- ${r.entryFile}`).join('\n')
)
