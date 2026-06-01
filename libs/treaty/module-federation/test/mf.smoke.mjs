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
import { readFile, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import {
	generateMfConfig,
	resolveFederation,
	isFederationEnabled,
	toRspackModuleFederation,
	toViteFederation,
	deriveExposesFromRoutes,
	deriveExposesFromLibs,
	exportMfConfig,
	writeMfConfig,
	renderMfConfigFile,
	exportFederationConfig,
	writeFederationConfig,
	renderFederationConfigFile,
	federatedModules,
	toFederatedModuleInputs,
	federatedModuleInputs,
	DEFAULT_HOST_NAME,
	DEFAULT_FILENAME,
} from '../dist/index.js'

let failures = 0
const results = []
const pending = []

function check(label, fn) {
	pending.push(
		(async () => {
			try {
				await fn()
				results.push(`PASS ${label}`)
			} catch (err) {
				failures++
				results.push(`FAIL ${label}: ${err.stack ?? err.message}`)
			}
		})()
	)
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

// 10. deriveExposesFromRoutes: two lazy routes -> two entries; eager excluded
check('deriveExposesFromRoutes exposes lazy routes only', () => {
	const exposes = deriveExposesFromRoutes([
		{ path: 'dashboard', loadComponent: () => ({}) },
		{ path: 'reports', loadChildren: () => ({}) },
		{ path: 'home', component: {} },
		{ path: '', redirectTo: 'dashboard', pathMatch: 'full' },
	])
	const keys = Object.keys(exposes)
	assert.equal(keys.length, 2, 'exactly two lazy routes exposed')
	assert.equal(exposes['./routes/dashboard'], './src/app/dashboard', 'loadComponent route exposed')
	assert.equal(exposes['./routes/reports'], './src/app/reports', 'loadChildren route exposed')
	assert.equal(exposes['./routes/home'], undefined, 'eager component route not exposed')
})

// 11. nested lazy route under an eager layout keeps its full path
check('deriveExposesFromRoutes walks nested eager routes', () => {
	const exposes = deriveExposesFromRoutes([
		{
			path: 'admin',
			component: {},
			children: [{ path: 'users', loadComponent: () => ({}) }],
		},
	])
	assert.equal(exposes['./routes/admin/users'], './src/app/admin/users', 'nested lazy route exposed at full path')
	assert.equal(Object.keys(exposes).length, 1, 'eager parent not exposed')
})

// 12. deriveExposesFromLibs: string + object entries
check('deriveExposesFromLibs exposes each library', () => {
	const exposes = deriveExposesFromLibs([
		'./libs/data-access',
		{ name: 'ui', path: './libs/shared/ui/index.ts' },
	])
	assert.equal(exposes['./libs/data-access'], './libs/data-access', 'string lib exposed by trailing segment')
	assert.equal(exposes['./libs/ui'], './libs/shared/ui/index.ts', 'object lib exposed by explicit name')
	assert.equal(Object.keys(exposes).length, 2, 'both libs exposed')
})

// 13. generateMfConfig with routes produces a host whose exposes includes them
check('generateMfConfig auto-exposes lazy routes + libs', () => {
	const cfg = generateMfConfig({
		name: 'shell',
		routes: [
			{ path: 'dashboard', loadComponent: () => ({}) },
			{ path: 'reports', loadChildren: () => ({}) },
			{ path: 'home', component: {} },
		],
		libs: ['./libs/data-access'],
	})
	assert.equal(cfg.name, 'shell', 'still a host')
	assert.equal(cfg.filename, DEFAULT_FILENAME, 'still emits a remote entry')
	assert.equal(cfg.exposes['./routes/dashboard'], './src/app/dashboard', 'lazy route auto-exposed')
	assert.equal(cfg.exposes['./routes/reports'], './src/app/reports', 'lazy children auto-exposed')
	assert.equal(cfg.exposes['./routes/home'], undefined, 'eager route not exposed')
	assert.equal(cfg.exposes['./libs/data-access'], './libs/data-access', 'lib auto-exposed')
	assert.ok(cfg.shared['@angular/core'], 'Angular singletons still shared')
})

// 14. manual exposes win over derived ones; backward compatible without routes
check('manual exposes win; no routes/libs is unchanged', () => {
	const overridden = generateMfConfig({
		routes: [{ path: 'dashboard', loadComponent: () => ({}) }],
		exposes: { './routes/dashboard': './custom/path.ts' },
	})
	assert.equal(overridden.exposes['./routes/dashboard'], './custom/path.ts', 'manual expose wins over derived')

	const bare = generateMfConfig({ name: 'plain' })
	assert.deepEqual(bare.exposes, {}, 'no routes/libs/exposes => empty exposes (backward compatible)')
})

// 15. derived exposes flow through the adapters
check('derived exposes reach the rspack adapter', () => {
	const opts = toRspackModuleFederation({
		name: 'shell',
		routes: [{ path: 'dashboard', loadComponent: () => ({}) }],
	})
	assert.equal(opts.exposes['./routes/dashboard'], './src/app/dashboard', 'derived expose forwarded to plugin options')
})

// 16. enabled:false yields an inert, disabled config (wiring stays in source)
check('enabled:false produces a disabled, inert config', () => {
	const cfg = generateMfConfig({
		enabled: false,
		name: 'shell',
		remotes: { dashboard: 'http://localhost:4201/remoteEntry.js' },
		routes: [{ path: 'dashboard', loadComponent: () => ({}) }],
		exposes: { './Widget': './src/app/widget.ts' },
		shared: { lodash: true },
	})
	assert.equal(cfg.enabled, false, 'config reports disabled')
	assert.equal(cfg.name, 'shell', 'identity still resolved')
	assert.equal(cfg.filename, DEFAULT_FILENAME, 'filename still resolved')
	assert.deepEqual(cfg.remotes, {}, 'no remotes when disabled')
	assert.deepEqual(cfg.exposes, {}, 'no exposes when disabled')
	assert.deepEqual(cfg.shared, {}, 'nothing shared when disabled (Angular suppressed too)')

	const enabled = generateMfConfig({ name: 'shell' })
	assert.equal(enabled.enabled, true, 'default is enabled (zero-config federated by default)')
	assert.ok(enabled.shared['@angular/core'], 'default still shares Angular')
})

// 17. exportMfConfig round-trips host/remotes/shared
check('exportMfConfig round-trips host/remotes/shared', () => {
	const options = {
		name: 'shell',
		remotes: { dashboard: 'http://localhost:4201/remoteEntry.js' },
		shared: { lodash: { singleton: true, eager: false } },
	}
	const exported = exportMfConfig(options)
	assert.equal(exported.enabled, true, 'enabled carried through')
	assert.equal(exported.name, 'shell', 'name exported')
	assert.equal(exported.filename, DEFAULT_FILENAME, 'filename exported')
	assert.deepEqual(
		exported.remotes['dashboard'],
		{ name: 'dashboard', entry: 'http://localhost:4201/remoteEntry.js' },
		'remote normalized + exported'
	)
	assert.ok(exported.shared['@angular/core'], 'Angular default exported in shared')
	assert.equal(exported.shared['lodash'].eager, false, 'user shared override exported')

	// Round-trip: the export is serializable and reproduces the normalized config.
	const roundTripped = JSON.parse(JSON.stringify(exported))
	const regenerated = exportMfConfig(
		generateMfConfig({
			name: roundTripped.name,
			filename: roundTripped.filename,
			remotes: Object.fromEntries(
				Object.entries(roundTripped.remotes).map(([alias, r]) => [alias, r])
			),
			shareAngular: false,
			shared: roundTripped.shared,
		})
	)
	assert.deepEqual(regenerated.remotes, exported.remotes, 'remotes survive a round-trip')
	assert.deepEqual(regenerated.shared, exported.shared, 'shared survives a round-trip')
})

// 18. exportMfConfig keys are sorted + free of undefined for a clean diff
check('exportMfConfig sorts keys and drops undefined', () => {
	const exported = exportMfConfig({
		remotes: { zebra: 'http://z/remoteEntry.js', alpha: 'http://a/remoteEntry.js' },
	})
	assert.deepEqual(Object.keys(exported.remotes), ['alpha', 'zebra'], 'remote keys sorted')
	const core = exported.shared['@angular/core']
	assert.equal('version' in core, false, 'no undefined version field leaks into the export')
})

// 19. a routes-derived config exports its exposes
check('exportMfConfig exports routes-derived exposes', () => {
	const exported = exportMfConfig({
		name: 'shell',
		routes: [
			{ path: 'dashboard', loadComponent: () => ({}) },
			{ path: 'reports', loadChildren: () => ({}) },
			{ path: 'home', component: {} },
		],
	})
	assert.equal(exported.exposes['./routes/dashboard'], './src/app/dashboard', 'lazy route exposed in export')
	assert.equal(exported.exposes['./routes/reports'], './src/app/reports', 'lazy children exposed in export')
	assert.equal(exported.exposes['./routes/home'], undefined, 'eager route not exposed')
})

// 20. exportMfConfig of a disabled config is inert
check('exportMfConfig reflects a disabled config', () => {
	const exported = exportMfConfig({ enabled: false, name: 'shell', remotes: { x: 'http://x/r.js' } })
	assert.equal(exported.enabled, false, 'export reports disabled')
	assert.deepEqual(exported.remotes, {}, 'disabled export has no remotes')
	assert.deepEqual(exported.shared, {}, 'disabled export shares nothing')
})

// 21. renderMfConfigFile emits JSON for a .json target
check('renderMfConfigFile emits JSON for .json paths', () => {
	const exported = exportMfConfig({ name: 'shell' })
	const json = renderMfConfigFile(exported, '/tmp/mf.config.json')
	const parsed = JSON.parse(json)
	assert.equal(parsed.name, 'shell', 'JSON file parses back to the config')
	assert.ok(parsed.shared['@angular/core'], 'shared present in JSON file')
})

// 22. renderMfConfigFile emits a re-importable TS module for .ts targets
check('renderMfConfigFile emits a TS module for .ts paths', () => {
	const exported = exportMfConfig({ name: 'shell' })
	const ts = renderMfConfigFile(exported, '/tmp/mf.config.ts')
	assert.ok(ts.includes("from '@treaty/module-federation'"), 'TS module imports the package')
	assert.ok(ts.includes('generateMfConfig('), 'TS module re-runs generateMfConfig')
	assert.ok(ts.includes('export default'), 'TS module has a default export')
})

// 23. writeMfConfig writes a JSON file to disk and returns the exported config
check('writeMfConfig ejects to disk', async () => {
	const dir = await (await import('node:fs/promises')).mkdtemp(path.join(os.tmpdir(), 'mf-eject-'))
	const dest = path.join(dir, 'nested', 'mf.config.json')
	try {
		const returned = await writeMfConfig({ name: 'shell', remotes: { d: 'http://d/r.js' } }, dest)
		const onDisk = JSON.parse(await readFile(dest, 'utf8'))
		assert.equal(onDisk.name, 'shell', 'name written to disk')
		assert.deepEqual(onDisk.remotes['d'], { name: 'd', entry: 'http://d/r.js' }, 'remote written to disk')
		assert.deepEqual(returned, onDisk, 'writeMfConfig returns the config it wrote')
	} finally {
		await rm(dir, { recursive: true, force: true })
	}
})

// 24. resolveFederation toggle: default ON, false/{enabled:false} OFF
check('resolveFederation reads the on/off toggle', () => {
	assert.deepEqual(resolveFederation(undefined), {}, 'undefined => on with default options')
	assert.deepEqual(resolveFederation(true), {}, 'true => on with default options')
	assert.equal(resolveFederation(false), null, 'false => off (null)')
	assert.equal(resolveFederation({ enabled: false }), null, '{enabled:false} => off (null)')
	const opts = { name: 'shell', remotes: { d: 'http://d/r.js' } }
	assert.equal(resolveFederation(opts), opts, 'options object => on, returned as-is')

	assert.equal(isFederationEnabled(undefined), true, 'default enabled')
	assert.equal(isFederationEnabled(false), false, 'false disabled')
	assert.equal(isFederationEnabled({ enabled: false }), false, '{enabled:false} disabled')
	assert.equal(isFederationEnabled({ name: 'x' }), true, 'options enabled')
})

// 25. adapters emit inert options for a disabled config (skip federation surface)
check('rspack/vite adapters honor enabled:false (inert options)', () => {
	const disabled = {
		enabled: false,
		name: 'shell',
		remotes: { dashboard: 'http://localhost:4201/remoteEntry.js' },
		routes: [{ path: 'dashboard', loadComponent: () => ({}) }],
		shared: { lodash: true },
	}
	const rspack = toRspackModuleFederation(disabled)
	assert.equal(rspack.name, 'shell', 'identity preserved when disabled')
	assert.deepEqual(rspack.remotes, {}, 'no remotes wired when disabled (rspack)')
	assert.deepEqual(rspack.exposes, {}, 'no exposes wired when disabled (rspack)')
	assert.deepEqual(rspack.shared, {}, 'nothing shared when disabled (rspack)')

	const vite = toViteFederation(disabled)
	assert.deepEqual(vite.remotes, {}, 'no remotes wired when disabled (vite)')
	assert.deepEqual(vite.shared, {}, 'nothing shared when disabled (vite)')

	// enabled (default) still wires through
	const on = toRspackModuleFederation({ name: 'shell', remotes: { d: 'http://d/r.js' } })
	assert.ok(on.shared['@angular/core'], 'enabled config still shares Angular')
	assert.equal(on.remotes['d'], 'd@http://d/r.js', 'enabled config still wires remotes')
})

// 26. exportFederationConfig: standalone enhanced config from an app graph
check('exportFederationConfig ejects a standalone enhanced config from an app graph', () => {
	const config = exportFederationConfig({
		name: 'shell',
		routes: [
			{ path: 'dashboard', loadComponent: () => ({}) },
			{ path: 'reports', loadChildren: () => ({}) },
			{ path: 'home', component: {} },
		],
		libs: ['./libs/data-access'],
		remotes: { reports_remote: 'http://localhost:4202/remoteEntry.js' },
		shared: { lodash: { singleton: true, eager: false } },
	})
	// host identity
	assert.equal(config.name, 'shell', 'host name')
	assert.equal(config.filename, DEFAULT_FILENAME, 'host filename')
	// remotes are name@entry strings (enhanced shape)
	assert.equal(
		config.remotes['reports_remote'],
		'reports_remote@http://localhost:4202/remoteEntry.js',
		'consumed remote rendered as name@entry string'
	)
	// lazy routes + libs => exposes (versioned remotes), eager route excluded
	assert.equal(config.exposes['./routes/dashboard'], './src/app/dashboard', 'lazy route exposed')
	assert.equal(config.exposes['./routes/reports'], './src/app/reports', 'lazy children exposed')
	assert.equal(config.exposes['./routes/home'], undefined, 'eager route not exposed')
	assert.equal(config.exposes['./libs/data-access'], './libs/data-access', 'lib exposed')
	// shared: Angular singletons + user override
	assert.equal(config.shared['@angular/core'].singleton, true, 'Angular singleton shared')
	assert.equal(config.shared['@angular/core'].eager, true, 'Angular eager')
	assert.equal(config.shared['lodash'].eager, false, 'user shared override carried')
	// deterministic: keys sorted, no undefined leaks
	assert.deepEqual(Object.keys(config.exposes), [...Object.keys(config.exposes)].sort(), 'exposes sorted')
	assert.equal('version' in config.shared['@angular/core'], false, 'no undefined version leaks')
	// matches the rspack adapter shape exactly (eject == what Treaty would wire)
	const viaAdapter = toRspackModuleFederation({
		name: 'shell',
		routes: [
			{ path: 'dashboard', loadComponent: () => ({}) },
			{ path: 'reports', loadChildren: () => ({}) },
			{ path: 'home', component: {} },
		],
		libs: ['./libs/data-access'],
		remotes: { reports_remote: 'http://localhost:4202/remoteEntry.js' },
		shared: { lodash: { singleton: true, eager: false } },
	})
	assert.deepEqual(config.exposes, viaAdapter.exposes, 'ejected exposes == adapter exposes')
	assert.deepEqual(config.remotes, viaAdapter.remotes, 'ejected remotes == adapter remotes')
})

// 27. exportFederationConfig round-trips to a serializable, valid config object
check('exportFederationConfig round-trips through JSON to a valid standalone config', () => {
	const config = exportFederationConfig({
		name: 'shell',
		routes: [{ path: 'dashboard', loadComponent: () => ({}) }],
		libs: ['./libs/ui'],
	})
	const round = JSON.parse(JSON.stringify(config))
	assert.deepEqual(round, config, 'config is plain JSON (no functions/undefined)')
	// expected remotes/exposes/shared survive
	assert.equal(round.exposes['./routes/dashboard'], './src/app/dashboard', 'route survives round-trip')
	assert.equal(round.exposes['./libs/ui'], './libs/ui', 'lib survives round-trip')
	assert.ok(round.shared['@angular/core'], 'shared survives round-trip')
	assert.equal(round.name, 'shell', 'name survives round-trip')
})

// 28. disabled app graph ejects an inert standalone config
check('exportFederationConfig of a disabled graph is inert', () => {
	const config = exportFederationConfig({
		enabled: false,
		name: 'shell',
		routes: [{ path: 'dashboard', loadComponent: () => ({}) }],
		remotes: { d: 'http://d/r.js' },
		libs: ['./libs/ui'],
	})
	assert.equal(config.name, 'shell', 'identity preserved')
	assert.deepEqual(config.remotes, {}, 'no remotes when disabled')
	assert.deepEqual(config.exposes, {}, 'no exposes when disabled')
	assert.deepEqual(config.shared, {}, 'nothing shared when disabled')
})

// 29. renderFederationConfigFile: JSON + a STANDALONE (Treaty-free) TS module
check('renderFederationConfigFile emits JSON and an import-free TS module', () => {
	const config = exportFederationConfig({ name: 'shell', libs: ['./libs/ui'] })

	const json = renderFederationConfigFile(config, '/tmp/federation.config.json')
	const parsed = JSON.parse(json)
	assert.equal(parsed.name, 'shell', 'JSON parses back to the config')
	assert.equal(parsed.exposes['./libs/ui'], './libs/ui', 'exposes present in JSON')

	const ts = renderFederationConfigFile(config, '/tmp/federation.config.ts')
	assert.equal(ts.includes('@treaty/'), false, 'standalone TS module has NO @treaty import (user owns it)')
	assert.equal(ts.includes('import '), false, 'standalone TS module is import-free')
	assert.ok(ts.includes('export const federationConfig ='), 'exports the config object literal')
	assert.ok(ts.includes('export default'), 'has a default export for ModuleFederationPlugin')
})

// 30. writeFederationConfig ejects a standalone config the user owns
check('writeFederationConfig ejects a standalone config to disk', async () => {
	const dir = await (await import('node:fs/promises')).mkdtemp(path.join(os.tmpdir(), 'fed-eject-'))
	const dest = path.join(dir, 'federation', 'federation.config.json')
	try {
		const returned = await writeFederationConfig(
			{
				name: 'shell',
				routes: [{ path: 'dashboard', loadComponent: () => ({}) }],
				libs: ['./libs/data-access'],
				remotes: { reports: 'http://r/remoteEntry.js' },
			},
			dest
		)
		const onDisk = JSON.parse(await readFile(dest, 'utf8'))
		assert.equal(onDisk.name, 'shell', 'name written to disk')
		assert.equal(onDisk.remotes['reports'], 'reports@http://r/remoteEntry.js', 'remote written as name@entry')
		assert.equal(onDisk.exposes['./routes/dashboard'], './src/app/dashboard', 'route expose written')
		assert.equal(onDisk.exposes['./libs/data-access'], './libs/data-access', 'lib expose written')
		assert.ok(onDisk.shared['@angular/core'], 'shared written')
		assert.deepEqual(returned, onDisk, 'writeFederationConfig returns the config it wrote')
	} finally {
		await rm(dir, { recursive: true, force: true })
	}
})

// 31. federatedModules: route graph -> deployable module descriptors (the D2 pass)
check('federatedModules derives host + lazy-route + lib modules from the route graph', () => {
	const modules = federatedModules({
		name: 'shell',
		routes: [
			{ path: 'dashboard', loadComponent: () => ({}) },
			{ path: 'reports', loadChildren: () => ({}) },
			{ path: 'home', component: {} }, // eager: not a module
			{ path: 'admin', component: {}, children: [{ path: 'users', loadComponent: () => ({}) }] },
		],
		libs: ['./libs/data-access'],
	})
	const byId = Object.fromEntries(modules.map((m) => [m.moduleId, m]))
	assert.equal(byId['shell'].kind, 'host', 'host container is a module')
	assert.equal(byId['shell'].path, DEFAULT_FILENAME, 'host backed by its remote entry filename')
	assert.equal(byId['./routes/dashboard'].kind, 'route', 'lazy route is a route module')
	assert.equal(byId['./routes/dashboard'].path, './src/app/dashboard', 'route backed by its exposed source path')
	assert.equal(byId['./routes/reports'].kind, 'route', 'lazy children is a route module')
	assert.equal(byId['./routes/admin/users'].kind, 'route', 'nested lazy route is a module at full path')
	assert.equal(byId['./libs/data-access'].kind, 'lib', 'lib is a lib module')
	assert.equal(byId['./routes/home'], undefined, 'eager route is NOT a module')
	assert.deepEqual(modules.map((m) => m.moduleId), [
		'./libs/data-access',
		'./routes/admin/users',
		'./routes/dashboard',
		'./routes/reports',
		'shell',
	], 'exactly the host + lazy routes + libs, sorted')
})

// 32. includeHost:false + a disabled config yields only/no host
check('federatedModules: includeHost toggle + disabled config', () => {
	const noHost = federatedModules({ name: 'shell', routes: [{ path: 'd', loadComponent: () => ({}) }] }, { includeHost: false })
	assert.deepEqual(noHost.map((m) => m.moduleId), ['./routes/d'], 'includeHost:false omits the host')

	const disabled = federatedModules({ enabled: false, name: 'shell', routes: [{ path: 'd', loadComponent: () => ({}) }] })
	assert.deepEqual(disabled.map((m) => m.moduleId), ['shell'], 'disabled config exposes nothing (host only)')
	assert.deepEqual(
		federatedModules({ enabled: false, name: 'shell', libs: ['./libs/x'] }, { includeHost: false }),
		[],
		'disabled + no host => empty'
	)
})

// 33. manual exposes participate as modules (manual wins over derived)
check('federatedModules includes manual exposes (classified by prefix)', () => {
	const modules = federatedModules({
		name: 'shell',
		exposes: { './widgets/clock': './src/app/clock.ts', './libs/manual': './libs/manual/index.ts' },
	})
	const byId = Object.fromEntries(modules.map((m) => [m.moduleId, m]))
	assert.equal(byId['./widgets/clock'].kind, 'route', 'non-lib manual expose classified as a route module')
	assert.equal(byId['./widgets/clock'].path, './src/app/clock.ts', 'manual expose path carried')
	assert.equal(byId['./libs/manual'].kind, 'lib', 'manual expose under ./libs classified as a lib module')
})

// 34. toFederatedModuleInputs / federatedModuleInputs -> buildManifest-ready seed
check('federatedModuleInputs builds a manifest-ready module seed from the route graph', () => {
	const inputs = federatedModuleInputs(
		{ name: 'shell', routes: [{ path: 'dashboard', loadComponent: () => ({}) }], libs: ['./libs/ui'] },
		{ version: '1.2.3', urlFor: (m, v) => `https://cdn/${m.moduleId}/${v}/remoteEntry.js` }
	)
	const byId = Object.fromEntries(inputs.map((i) => [i.moduleId, i]))
	// host + route + lib, each stamped with the version + derived url + kind (the
	// exact FederatedModuleInput shape @treaty/federation-deploy.buildManifest consumes).
	assert.deepEqual(byId['shell'], { moduleId: 'shell', version: '1.2.3', url: 'https://cdn/shell/1.2.3/remoteEntry.js', kind: 'host' })
	assert.deepEqual(byId['./routes/dashboard'], {
		moduleId: './routes/dashboard', version: '1.2.3', url: 'https://cdn/./routes/dashboard/1.2.3/remoteEntry.js', kind: 'route',
	})
	assert.equal(byId['./libs/ui'].kind, 'lib', 'lib kind carried into the input')

	// defaults: placeholder version + relative versioned path.
	const def = toFederatedModuleInputs(federatedModules({ name: 'app' }))
	const host = def.find((i) => i.moduleId === 'app')
	assert.equal(host.version, '0.0.0', 'default placeholder version')
	assert.equal(host.url, 'app/0.0.0/remoteEntry.js', 'default relative versioned path')
	assert.equal(host.kind, 'host', 'default host kind')
})

await Promise.all(pending)
for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
