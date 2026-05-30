/**
 * Node smoke test for @treaty/module-federation.
 *
 * Exercises the public surface against the built dist (which is what the
 * bundler plugins consume). It asserts:
 *   1. generateMfConfig({}) produces a valid HOST config (name + filename) with
 *      the @angular/* runtime shared as EAGER SINGLETONS,
 *   2. rxjs/tslib/zone.js are also eager singletons,
 *   3. an app with declared remotes wires them (alias -> { name, entry }),
 *   4. exposes are carried through,
 *   5. user `shared` overrides win over the Angular defaults; shareAngular:false
 *      drops the Angular singletons,
 *   6. toRspackModuleFederation returns plugin-ready @module-federation/enhanced
 *      options (remotes as `name@entry` strings, shared keyed by package),
 *   7. toViteFederation returns plugin-ready @module-federation/vite options,
 *   8. both adapters accept either raw MfOptions or a normalized config,
 *   9. a { name, entry } remote object is honoured verbatim.
 *
 * Run: node libs/treaty/module-federation/test/mf.smoke.mjs
 */

import assert from 'node:assert/strict'
import {
	generateMfConfig,
	toRspackModuleFederation,
	toViteFederation,
	DEFAULT_HOST_NAME,
	DEFAULT_FILENAME,
} from '../dist/index.js'

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

// 1. zero-config host with Angular eager singletons
check('generateMfConfig({}) -> host with @angular eager singletons', () => {
	const cfg = generateMfConfig()
	assert.equal(cfg.name, DEFAULT_HOST_NAME, 'defaults to host name')
	assert.equal(cfg.filename, DEFAULT_FILENAME, 'always emits a remote entry filename')
	const core = cfg.shared['@angular/core']
	assert.ok(core, '@angular/core is shared by default')
	assert.equal(core.singleton, true, '@angular/core is a singleton')
	assert.equal(core.eager, true, '@angular/core is eager')
	assert.ok(core.requiredVersion, '@angular/core carries a requiredVersion')
	assert.ok(cfg.shared['@angular/common'], '@angular/common shared')
	assert.ok(cfg.shared['@angular/router'], '@angular/router shared')
})

// 2. non-angular runtime singletons
check('rxjs/tslib/zone.js shared as eager singletons', () => {
	const cfg = generateMfConfig()
	for (const pkg of ['rxjs', 'tslib', 'zone.js']) {
		assert.ok(cfg.shared[pkg], `${pkg} shared`)
		assert.equal(cfg.shared[pkg].singleton, true, `${pkg} singleton`)
		assert.equal(cfg.shared[pkg].eager, true, `${pkg} eager`)
	}
})

// 3. declared remotes wire through
check('declared remotes wire to { name, entry }', () => {
	const cfg = generateMfConfig({
		name: 'shell',
		remotes: { dashboard: 'http://localhost:4201/remoteEntry.js' },
	})
	assert.equal(cfg.name, 'shell', 'app name honoured')
	const r = cfg.remotes['dashboard']
	assert.ok(r, 'dashboard remote present')
	assert.equal(r.name, 'dashboard', 'alias becomes the federation name by default')
	assert.equal(r.entry, 'http://localhost:4201/remoteEntry.js', 'entry url preserved')
})

// 4. exposes carry through
check('exposes are carried through', () => {
	const cfg = generateMfConfig({ exposes: { './Widget': './src/app/widget.ts' } })
	assert.equal(cfg.exposes['./Widget'], './src/app/widget.ts', 'expose path preserved')
})

// 5. user shared overrides + shareAngular:false
check('user shared overrides win; shareAngular:false drops Angular', () => {
	const overridden = generateMfConfig({
		shared: { '@angular/core': { singleton: true, eager: false } },
	})
	assert.equal(overridden.shared['@angular/core'].eager, false, 'user override wins over default')

	const none = generateMfConfig({ shareAngular: false, shared: { lodash: true } })
	assert.equal(none.shared['@angular/core'], undefined, 'no Angular singletons when shareAngular:false')
	assert.ok(none.shared['lodash'], 'explicit shared still present')
	assert.equal(none.shared['lodash'].singleton, true, 'shared:true => eager singleton')
})

// 6. rspack adapter -> plugin-ready options
check('toRspackModuleFederation returns plugin-ready options', () => {
	const opts = toRspackModuleFederation({
		name: 'shell',
		remotes: { dashboard: 'http://localhost:4201/remoteEntry.js' },
		exposes: { './Header': './src/app/header.ts' },
	})
	assert.equal(opts.name, 'shell', 'name present')
	assert.equal(typeof opts.filename, 'string', 'filename present')
	assert.equal(
		opts.remotes['dashboard'],
		'dashboard@http://localhost:4201/remoteEntry.js',
		'remote rendered as name@entry string'
	)
	assert.equal(opts.exposes['./Header'], './src/app/header.ts', 'expose forwarded')
	assert.ok(opts.shared['@angular/core'], 'shared keyed by package name')
	assert.equal(opts.shared['@angular/core'].singleton, true, 'singleton forwarded')
	assert.equal(opts.shared['@angular/core'].eager, true, 'eager forwarded')
})

// 7. vite adapter -> plugin-ready options
check('toViteFederation returns plugin-ready options', () => {
	const opts = toViteFederation({
		name: 'shell',
		remotes: { dashboard: 'http://localhost:4201/remoteEntry.js' },
	})
	assert.equal(opts.name, 'shell', 'name present')
	assert.equal(typeof opts.filename, 'string', 'filename present')
	assert.equal(
		opts.remotes['dashboard'],
		'dashboard@http://localhost:4201/remoteEntry.js',
		'remote rendered as name@entry string'
	)
	assert.ok(opts.shared['@angular/core'], 'shared keyed by package name')
	assert.equal(opts.shared['@angular/core'].singleton, true, 'singleton forwarded')
})

// 8. adapters accept a pre-normalized config
check('adapters accept a normalized config without re-defaulting', () => {
	const normalized = generateMfConfig({ name: 'pre' })
	const rspack = toRspackModuleFederation(normalized)
	const vite = toViteFederation(normalized)
	assert.equal(rspack.name, 'pre', 'rspack uses normalized name')
	assert.equal(vite.name, 'pre', 'vite uses normalized name')
	assert.ok(rspack.shared['@angular/core'], 'rspack keeps shared from normalized')
	assert.ok(vite.shared['@angular/core'], 'vite keeps shared from normalized')
})

// 9. { name, entry } remote object honoured
check('{ name, entry } remote object is honoured verbatim', () => {
	const opts = toRspackModuleFederation({
		remotes: { dash: { name: 'dashboard_app', entry: 'http://localhost:4201/remoteEntry.js' } },
	})
	assert.equal(
		opts.remotes['dash'],
		'dashboard_app@http://localhost:4201/remoteEntry.js',
		'explicit federation name used in the name@entry string'
	)
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
