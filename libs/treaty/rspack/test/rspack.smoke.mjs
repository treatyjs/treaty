/**
 * Node smoke test for @treaty/rspack — full feature PARITY with @treaty/vite.
 *
 * Does NOT run a full Rspack build (the bundler is a peer dependency and is not
 * installed). Instead it drives the contracts this package owns directly — the
 * loader, the plugin's `apply`, and the shared linker/routes cores — and asserts
 * the four feature areas a Treaty app needs to build on Rspack identically to Vite:
 *
 *   1a. AUTHORING LOWERING of the Treaty-owned extensions (`.treaty`/`.tsx`/`.tjsx`)
 *       through `@treaty/compiler` -> Ivy JS.
 *   1b. AUTHORING LOWERING of plain `.ts` Angular decorated classes — `@Component`,
 *       `@Directive`, `@Pipe`, `@Injectable`, `@NgModule` — to Ivy (the gap this
 *       phase closes: DEFAULT_TEST now matches `.ts`, never `.d.ts`, and the loader
 *       classifies/passes a non-Angular `.ts` straight through).
 *   2.  PARTIAL-ANGULAR LINK over a real published `@angular/common` fesm chunk via
 *       the shared `linkPartialLoader` -> ZERO residual `ɵɵngDeclare`, no
 *       `@angular/compiler`.
 *   3.  ROUTES VIRTUAL MODULE: the plugin aliases `virtual:treaty-routes` to the
 *       in-package sentinel and the routes loader produces a consumable route graph.
 *
 * The MF peer (`@module-federation/enhanced`) is ABSENT in this monorepo, so the
 * plugin's clean-failure contract is asserted instead of a fabricated federated
 * build: with `moduleFederation` left default-on and the peer missing, `apply`
 * must NOT throw, must still wire the loader/link/routes rules, and must warn.
 *
 * Run: node libs/treaty/rspack/test/rspack.smoke.mjs
 */

import assert from 'node:assert/strict'
import { isAbsolute } from 'node:path'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import {
	treatyLoader,
	loaderPath,
	TreatyRspackPlugin,
	treatyRule,
	linkPartialRule,
	linkPartialLoader,
	isPartialModule,
	DEFAULT_TEST,
	DEFAULT_EXTENSIONS,
	TREATY_ROUTES_ID,
	routesSentinelPath,
	routesLoader,
} from '../dist/index.js'

/** The real example app whose routes/ tree the routes loader lowers. */
const EXAMPLE_ROOT = fileURLToPath(new URL('../../../../examples/file-routed-app', import.meta.url))

/** A real published partial Angular fesm chunk to link end-to-end. */
const ANGULAR_FIXTURE = fileURLToPath(
	new URL('../../../../node_modules/@angular/common/fesm2022/_location-chunk.mjs', import.meta.url)
)

let failures = 0
const results = []

function check(label, fn) {
	try {
		fn()
		results.push(`PASS ${label}`)
	} catch (err) {
		failures++
		results.push(`FAIL ${label}: ${err.stack ?? err.message}`)
	}
}

/** Lower `source` through the loader under a minimal (callback-less) context. */
function lower(resourcePath, source) {
	return treatyLoader.call({ resourcePath }, source)
}

// ---------------------------------------------------------------------------
// 1a. AUTHORING LOWERING of the Treaty-owned extensions.
// ---------------------------------------------------------------------------

check('loader lowers .treaty to Ivy JS', () => {
	const out = lower('logo.treaty', '<div>hello</div>\n')
	assert.ok(out.includes('defineComponent'), 'emitted Ivy JS must contain defineComponent')
})

check('loader lowers .tsx (@Component) and bare-JSX .tsx to Ivy JS', () => {
	const decorated =
		"import { Component } from '@angular/core'\n" +
		"@Component({ selector: 'app-x', template: '<div>x</div>' })\n" +
		'export class XComponent {}\n'
	assert.ok(lower('x.tsx', decorated).includes('defineComponent'), 'decorated .tsx lowers')
	const bare = 'export default function App() {\n  return <main>bare</main>\n}\n'
	assert.ok(lower('App.tsx', bare).includes('defineComponent'), 'bare-JSX .tsx lowers')
})

check('loader lowers .tjsx to Ivy JS', () => {
	const out = lower('hero.tjsx', 'export default () => <footer>hi</footer>\n')
	assert.ok(out.includes('defineComponent'), '.tjsx must lower to Ivy JS')
})

// ---------------------------------------------------------------------------
// 1b. AUTHORING LOWERING of plain `.ts` Angular decorated classes (the gap).
// ---------------------------------------------------------------------------

check('DEFAULT_TEST matches .ts/.treaty/.tsx/.tjsx but never .d.ts or .js', () => {
	assert.ok(DEFAULT_TEST.test('app.component.ts'), 'matches .ts')
	assert.ok(DEFAULT_TEST.test('x.treaty'), 'matches .treaty')
	assert.ok(DEFAULT_TEST.test('x.tsx'), 'matches .tsx')
	assert.ok(DEFAULT_TEST.test('x.tjsx'), 'matches .tjsx')
	assert.ok(!DEFAULT_TEST.test('app.d.ts'), 'must NOT match .d.ts')
	assert.ok(!DEFAULT_TEST.test('app.js'), 'must NOT match .js')
	assert.ok(!DEFAULT_TEST.test('app.mjs'), 'must NOT match .mjs')
})

check('loader lowers every .ts Angular decorator kind to Ivy', () => {
	const kinds = {
		Component:
			"import { Component } from '@angular/core'\n@Component({ selector: 'a-x', template: '<i>x</i>' })\nexport class XComponent {}\n",
		Directive:
			"import { Directive } from '@angular/core'\n@Directive({ selector: '[appX]' })\nexport class XDir {}\n",
		Pipe: "import { Pipe } from '@angular/core'\n@Pipe({ name: 'x' })\nexport class XPipe { transform(v) { return v } }\n",
		Injectable:
			"import { Injectable } from '@angular/core'\n@Injectable({ providedIn: 'root' })\nexport class XSvc {}\n",
		NgModule: "import { NgModule } from '@angular/core'\n@NgModule({})\nexport class XMod {}\n",
	}
	for (const [kind, src] of Object.entries(kinds)) {
		const out = lower(`${kind.toLowerCase()}.ts`, src)
		assert.ok(out !== src, `${kind} .ts must be rewritten (lowered, not passed through)`)
		assert.ok(/ɵɵdefine|ɵfac/.test(out), `${kind} .ts must lower to an Ivy ɵɵdefine*/ɵfac`)
	}
})

check('loader passes a non-Angular .ts straight through unchanged', () => {
	const plain = 'export const answer = 42\nexport function add(a, b) { return a + b }\n'
	assert.equal(lower('util.ts', plain), plain, 'plain .ts must reach the host TS pipeline untouched')
	// A `.d.ts` (never matched by DEFAULT_TEST, but the loader must still no-op on it).
	const dts = 'export declare const y: number\n'
	assert.equal(lower('types.d.ts', dts), dts, '.d.ts must be untouched')
})

check('treatyRule carries the widened .ts test and points at the loader', () => {
	const rule = treatyRule()
	assert.equal(rule.test, DEFAULT_TEST, 'rule reuses the widened DEFAULT_TEST')
	assert.ok(rule.test.test('a.ts'), 'rule matches .ts')
	assert.equal(rule.use[0].loader, loaderPath, 'rule points at the treaty loader')
})

// ---------------------------------------------------------------------------
// 2. PARTIAL-ANGULAR LINK over a real published @angular/common fesm chunk.
// ---------------------------------------------------------------------------

check('linkPartialLoader links a real @angular/common chunk to zero residual ngDeclare', () => {
	const source = readFileSync(ANGULAR_FIXTURE, 'utf8')
	assert.ok(source.includes('ɵɵngDeclare'), 'fixture must actually be partial-compiled')
	assert.ok(isPartialModule(ANGULAR_FIXTURE, source), 'isPartialModule must flag the chunk')
	const linked = linkPartialLoader.call({ resourcePath: ANGULAR_FIXTURE }, source)
	assert.ok(typeof linked === 'string' && linked.length > 0, 'loader must emit linked source')
	assert.notEqual(linked, source, 'partial source must be rewritten')
	assert.equal((linked.match(/ɵɵngDeclare/g) || []).length, 0, 'ZERO residual ɵɵngDeclare')
	assert.ok(/ɵɵdefine/.test(linked), 'linked output contains AOT ɵɵdefine* calls')
	assert.ok(!linked.includes('@angular/compiler'), 'linked output must NOT need @angular/compiler')
})

check('linkPartialLoader passes a first-party / non-partial module through', () => {
	const code = 'export const x = 1\n'
	assert.equal(linkPartialLoader.call({ resourcePath: '/x/src/app.mjs' }, code), code, 'unchanged')
})

check('linkPartialRule matches node_modules .mjs and not first-party .mjs', () => {
	const rule = linkPartialRule()
	assert.equal(rule.type, 'javascript/auto', 'linker rule applies to ESM .mjs')
	assert.ok(rule.test.test('/x/node_modules/@angular/common/fesm2022/common.mjs'), 'matches vendor')
	assert.ok(!rule.test.test('/x/src/app.mjs'), 'does not match first-party')
})

// ---------------------------------------------------------------------------
// 3. ROUTES VIRTUAL MODULE — plugin wiring + loader generation.
// ---------------------------------------------------------------------------

check('plugin.apply wires the routes alias + loader rule when fileRoutes is set', () => {
	const plugin = new TreatyRspackPlugin({
		moduleFederation: false,
		fileRoutes: { routesRoot: EXAMPLE_ROOT },
	})
	const config = { module: {}, resolve: {} }
	plugin.apply({ options: config })
	assert.equal(config.resolve.alias[TREATY_ROUTES_ID], routesSentinelPath, 'virtual id aliased')
	const rule = config.module.rules.find(
		(r) => r.use && r.use[0] && /routes-virtual-loader\.js$/.test(r.use[0].loader)
	)
	assert.ok(rule, 'a routes loader rule must be wired')
	assert.equal(rule.use[0].options.routesRoot, EXAMPLE_ROOT, 'loader carries routesRoot')
})

check('routes loader generates a consumable route graph over the real example app', () => {
	const deps = []
	let cbErr
	let code
	routesLoader.call({
		getOptions: () => ({ routesRoot: EXAMPLE_ROOT }),
		addDependency: (f) => deps.push(f),
		callback: (err, content) => {
			cbErr = err
			code = content
		},
	})
	assert.ok(!cbErr, `loader must not error: ${cbErr && cbErr.message}`)
	assert.ok(code.includes('export default routes'), 'emits a default-exported routes module')
	assert.ok(code.includes('loadComponent'), 'lazy route loaders present')
	assert.ok(deps.length > 0 && deps.every(isAbsolute), 'route files registered as absolute deps')
})

// ---------------------------------------------------------------------------
// MF PEER ABSENT — clean-failure contract (no fabricated federated build).
// ---------------------------------------------------------------------------

check('plugin.apply with default-on MF + absent peer wires everything and does not throw', () => {
	const warnings = []
	const realWarn = console.warn
	console.warn = (...args) => warnings.push(args.join(' '))
	try {
		// moduleFederation left to its default (on) — the @module-federation/enhanced peer
		// is NOT installed in this monorepo, so apply() must degrade cleanly.
		const plugin = new TreatyRspackPlugin({ fileRoutes: { routesRoot: EXAMPLE_ROOT } })
		const config = { module: {}, resolve: {}, plugins: [] }
		assert.doesNotThrow(() => plugin.apply({ options: config }), 'apply must not throw without the MF peer')
		// The non-peer wiring is fully present regardless of MF.
		assert.ok(
			config.module.rules.some((r) => r.use && r.use[0] && r.use[0].loader === loaderPath),
			'authoring loader rule still wired'
		)
		assert.ok(
			config.module.rules.some((r) => r.type === 'javascript/auto' && r.test.test('/n/node_modules/@angular/core/x.mjs')),
			'link-partial rule still wired'
		)
		for (const ext of DEFAULT_EXTENSIONS) {
			assert.ok(config.resolve.extensions.includes(ext), `resolve.extensions includes ${ext}`)
		}
		// No federation plugin could be added (peer missing) -> a warning, no MF plugin.
		assert.equal(config.plugins.length, 0, 'no federation plugin added when the peer is absent')
		assert.ok(
			warnings.some((w) => w.includes('@module-federation/enhanced')),
			'a clean warning names the missing MF peer'
		)
	} finally {
		console.warn = realWarn
	}
})

check('plugin.apply with moduleFederation:false adds no federation plugin and no warning', () => {
	const warnings = []
	const realWarn = console.warn
	console.warn = (...args) => warnings.push(args.join(' '))
	try {
		const plugin = new TreatyRspackPlugin({ moduleFederation: false })
		const config = { module: {}, resolve: {}, plugins: [] }
		plugin.apply({ options: config })
		assert.equal(config.plugins.length, 0, 'opt-out adds no federation plugin')
		assert.equal(warnings.length, 0, 'opt-out emits no MF warning')
	} finally {
		console.warn = realWarn
	}
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
