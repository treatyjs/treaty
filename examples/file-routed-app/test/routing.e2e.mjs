// End-to-end proof that file-based routing is produced DURING the build as a
// VIRTUAL MODULE — never a checked-in / prebuilt routes.ts, and with no prebuild
// step.
//
// This drives the REAL `@treaty/vite` plugin (its `fileRoutes` option) over THIS
// app's own routes/ + api/ tree, exactly as a `vite build` / `vite` dev server
// would: the plugin's `resolveId('virtual:treaty-routes')` claims the virtual id
// and `load()` generates the Angular route module on the fly by driving the Rust
// file-routing core (`@treaty/authoring-node`.`generateRoutes`, the shim over the
// `treaty_file_routing` crate) against the filesystem. There is no hand-written
// table and no generated file on disk — if a route file is added/renamed/removed,
// the next `load()` reflects it. The route graph is the engine's output over the
// current tree.
//
// Three things are asserted:
//   1. NO prebuilt artifact exists on disk: no src/generated/routes.ts and no
//      scripts/generate-routes.mjs (the prebuild step is gone).
//   2. A build-time `load()` of the virtual module produces the expected routes,
//      Angular-native :params, federation remotes, and absolute lazy loaders from
//      the on-disk tree, and registers the route entry files as watch deps.
//   3. Dev regen: ADDING a new route file under routes/ and re-running `load()`
//      (after the plugin invalidates the module via handleHotUpdate) yields a
//      route graph that now includes the new route — no prebuild, no restart.
//
// Run: node test/routing.e2e.mjs   (or `npm run test:e2e`).
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import assert from 'node:assert/strict'

import treaty from '@treaty/vite'

const here = dirname(fileURLToPath(import.meta.url))
const appRoot = join(here, '..')
// Vite resolves module ids with POSIX separators, so the absolute import base the
// emitted lazy loaders use must be forward-slashed even on Windows.
const importBase = appRoot.replace(/\\/g, '/')

const ROUTES_ID = 'virtual:treaty-routes'
const RESOLVED_ID = '\0virtual:treaty-routes'
const ROUTES_PLUGIN = 'treaty:vite:file-routes'

/** Tiny test harness: collect pass/fail without extra deps. */
let failed = 0
const log = []
function test(name, fn) {
	try {
		fn()
		log.push(`OK   ${name}`)
	} catch (err) {
		failed++
		log.push(`FAIL ${name}\n  ${String(err.message).split('\n').join('\n  ')}`)
	}
}

/**
 * Build the `@treaty/vite` plugin set for this app (mirroring vite.config.ts) and
 * return the file-routes plugin, having run its `configResolved` lifecycle so the
 * build root is captured.
 */
function fileRoutesPlugin() {
	const plugins = treaty({
		fileRoutes: { routesRoot: appRoot, dynamicSegmentStyle: 'colon', importBase },
	})
	const plugin = plugins.find((p) => p && p.name === ROUTES_PLUGIN)
	assert.ok(plugin, `the @treaty/vite plugin set must include the ${ROUTES_PLUGIN} plugin`)
	plugin.configResolved.call({}, { root: appRoot, command: 'build' })
	return plugin
}

/** Generate the virtual routes module exactly as the bundler would, capturing the watch files. */
function loadRoutesModule(plugin) {
	const resolved = plugin.resolveId.call({}, ROUTES_ID)
	assert.equal(resolved, RESOLVED_ID, 'resolveId maps the bare virtual id to the resolved virtual id')
	const watched = []
	const out = plugin.load.call({ addWatchFile: (f) => watched.push(f) }, resolved)
	assert.ok(out && typeof out.code === 'string', 'load returns a { code } module')
	return { code: out.code, watched }
}

// Assert a `path: "<p>"` route with its absolute lazy `import("<importBase>/<entryFile>")` loader.
function hasRoute(code, path, entryFile) {
	const pathLine = `path: ${JSON.stringify(path)}`
	assert.ok(code.includes(pathLine), `missing route ${JSON.stringify(path)} (${pathLine})`)
	const loader = `import(${JSON.stringify(`${importBase}/${entryFile}`)})`
	assert.ok(code.includes(loader), `missing absolute lazy loader ${loader} for route ${JSON.stringify(path)}`)
}

// --- 1. The prebuilt artifact and prebuild step are GONE -----------------------

test('no checked-in / prebuilt routes.ts exists on disk', () => {
	assert.ok(
		!existsSync(join(appRoot, 'src', 'generated', 'routes.ts')),
		'src/generated/routes.ts must not exist — routing is a build-time virtual module, not a prebuilt file',
	)
})

test('no generate-routes prebuild script exists', () => {
	assert.ok(
		!existsSync(join(appRoot, 'scripts', 'generate-routes.mjs')),
		'scripts/generate-routes.mjs must not exist — there is no prebuild step',
	)
})

test('the app bootstrap imports the virtual module, not a generated file', () => {
	const main = readMain()
	assert.ok(main.includes("from 'virtual:treaty-routes'"), 'main.ts must import routes from virtual:treaty-routes')
	assert.ok(!main.includes('./generated/routes'), 'main.ts must not import a prebuilt ./generated/routes module')
})

// --- 2. A build-time load() produces routes straight from the filesystem -------

const built = loadRoutesModule(fileRoutesPlugin())

test('the virtual module is the engine-generated artifact built from the FS', () => {
	assert.ok(
		built.code.startsWith('// GENERATED by Treaty file routing'),
		'the virtual module must be produced by the file-routing engine, not a hand-written file',
	)
	// The shared shim down-levels the engine's TypeScript emit to plain JS so every
	// bundler can parse the virtual module directly (it has no on-disk path and so
	// bypasses a bundler's built-in TS transform): the route graph is intact but the
	// TS-only type surface (`import type`, the `: Routes` annotation, `as const`) is
	// stripped, so the export is the JS `export const routes = [`.
	assert.ok(built.code.includes('export const routes = ['), 'missing the Angular routes export')
	assert.ok(built.code.includes('export default routes'), 'missing default export of routes')
	assert.ok(!built.code.includes('import type'), 'served module is JS: no TS-only import type')
	assert.ok(!/\bconst routes\s*:/.test(built.code), 'served module is JS: no TS type annotation')
})

test('root layout + index map to the root layout/index files at path ""', () => {
	hasRoute(built.code, '', 'routes/layout.treaty')
	hasRoute(built.code, '', 'routes/index.treaty')
})

test('(marketing) route group is STRIPPED from the URL', () => {
	hasRoute(built.code, '', 'routes/(marketing)/index.treaty')
	hasRoute(built.code, 'about', 'routes/(marketing)/about.tjsx')
	assert.ok(
		!/path:\s*"[^"]*\(marketing\)[^"]*"/.test(built.code),
		'a route path still contains the (marketing) group name — it must be stripped',
	)
})

test('blog layout nests its children under "blog"', () => {
	hasRoute(built.code, 'blog', 'routes/blog/layout.treaty')
	hasRoute(built.code, '', 'routes/blog/index.treaty')
})

test('blog/[slug] lowers to the Angular param :slug', () => {
	hasRoute(built.code, ':slug', 'routes/blog/[slug]/index.treaty')
})

test('blog/[...path] catch-all directory lowers to a rest segment', () => {
	hasRoute(built.code, ':...path', 'routes/blog/[...path]/index.treaty')
})

test('deep nested docs/[category]/[page] lowers to docs/:category/:page', () => {
	hasRoute(built.code, 'docs/:category/:page', 'routes/docs/[category]/[page]/index.tjsx')
})

test('not-found lowers to the ** wildcard route', () => {
	hasRoute(built.code, '**', 'routes/not-found.treaty')
})

test('federation remotes carry the unique de-duplicated names', () => {
	assert.ok(built.code.includes('export const federationRemotes = ['), 'missing federationRemotes export')
	for (const name of [
		'root',
		'root-index',
		'root-marketing',
		'about',
		'blog',
		'root-blog',
		'path',
		'slug',
		'docs-category-page',
		'not-found',
	]) {
		assert.ok(built.code.includes(`"name": ${JSON.stringify(name)}`), `missing federation remote "${name}"`)
	}
	assert.ok(built.code.includes('"exposedModule": "./Route"'), 'remotes must expose ./Route')
})

test('every referenced route file is registered as a watch dependency', () => {
	assert.ok(built.watched.length > 0, 'route entry files must be registered via addWatchFile')
	for (const entry of [
		'routes/layout.treaty',
		'routes/index.treaty',
		'routes/blog/[slug]/index.treaty',
		'routes/docs/[category]/[page]/index.tjsx',
	]) {
		assert.ok(
			built.watched.some((f) => f.replace(/\\/g, '/').endsWith(entry)),
			`route file ${entry} must be a watch dependency`,
		)
	}
})

// --- 3. Dev regen: adding a route regenerates the virtual module ---------------

test('adding a new route file regenerates the virtual module (no prebuild)', () => {
	const newDir = join(appRoot, 'routes', '__e2e_temp')
	const newFile = join(newDir, 'index.treaty')
	const before = loadRoutesModule(fileRoutesPlugin())
	assert.ok(
		!before.code.includes('path: "__e2e_temp"'),
		'the temp route must not exist before the file is added',
	)

	mkdirSync(newDir, { recursive: true })
	writeFileSync(newFile, 'export class TempRoute {}\n')
	try {
		// Mirror dev: a route ADD flows through handleHotUpdate, which invalidates the
		// virtual module so the next load() regenerates the graph.
		const plugin = fileRoutesPlugin()
		let invalidated = false
		plugin.handleHotUpdate.call(
			{},
			{
				modules: [],
				server: {
					moduleGraph: {
						getModuleById: (id) => (id === RESOLVED_ID ? { id: RESOLVED_ID } : undefined),
						invalidateModule: () => (invalidated = true),
					},
					ws: { send: () => {} },
				},
			},
		)
		assert.ok(invalidated, 'handleHotUpdate must invalidate the routes virtual module on a route change')

		const after = loadRoutesModule(plugin)
		hasRoute(after.code, '__e2e_temp', 'routes/__e2e_temp/index.treaty')
	} finally {
		rmSync(newDir, { recursive: true, force: true })
	}

	// And once removed, the route graph drops it again — no stale prebuilt table.
	const restored = loadRoutesModule(fileRoutesPlugin())
	assert.ok(
		!restored.code.includes('path: "__e2e_temp"'),
		'removing the route file must drop the route from the regenerated graph',
	)
})

function readMain() {
	return readFileSync(join(appRoot, 'src', 'main.ts'), 'utf8')
}

console.log(log.join('\n'))
console.log(failed ? `\n${failed} test(s) failed` : `\nAll ${log.length} routing e2e checks passed`)
process.exit(failed ? 1 : 0)
