/**
 * Node smoke test for @treaty/schematics.
 *
 * Drives the BUILT dist collection through @angular-devkit/schematics' own
 * SchematicTestRunner (the same engine `ng add`/`ng generate` use), so it
 * exercises the real wiring a developer hits. It asserts:
 *
 *   1. collection.json is shaped correctly and registers ng-add, application,
 *      and library against compiled factories;
 *   2. ng-add rewrites a fixture angular.json's build/serve targets to the
 *      @treaty/build builders, makes the project a federation host
 *      (federation.config.json with @angular eager singletons), and scaffolds +
 *      wires a sample remote;
 *   3. ng-add --skipRemote converts builders only (no sample remote);
 *   4. generate application (host) produces the expected files, registers the
 *      project on the Treaty builders, and writes a host federation config;
 *   5. generate application --host=false produces an exposable remote;
 *   6. generate library produces an exposable remote library.
 *
 * Run: node libs/treaty/schematics/test/schematics.smoke.mjs
 */

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

import { SchematicTestRunner } from '@angular-devkit/schematics/testing/index.js'
import { HostTree } from '@angular-devkit/schematics/index.js'

const here = dirname(fileURLToPath(import.meta.url))
const collectionPath = join(here, '..', 'dist', 'collection.json')

let failures = 0
const results = []

function check(label, fn) {
	return Promise.resolve()
		.then(fn)
		.then(() => results.push(`PASS ${label}`))
		.catch((err) => {
			failures++
			results.push(`FAIL ${label}: ${err.stack ?? err.message}`)
		})
}

/** A minimal but realistic angular.json with a single app on the stock builders. */
function fixtureWorkspace() {
	return {
		version: 1,
		newProjectRoot: 'projects',
		projects: {
			shell: {
				projectType: 'application',
				root: '',
				sourceRoot: 'src',
				prefix: 'app',
				architect: {
					build: {
						builder: '@angular/build:application',
						options: { browser: 'src/main.ts' },
					},
					serve: {
						builder: '@angular/build:dev-server',
						options: {},
					},
					'extract-i18n': {
						builder: '@angular/build:extract-i18n',
						options: {},
					},
				},
			},
		},
	}
}

function newWorkspaceTree(workspace = fixtureWorkspace()) {
	const tree = new HostTree()
	tree.create('/angular.json', JSON.stringify(workspace, null, 2))
	return tree
}

function readJson(tree, path) {
	const buf = tree.read(path)
	assert.ok(buf, `expected ${path} to exist`)
	return JSON.parse(buf.toString('utf-8'))
}

const runner = new SchematicTestRunner('@treaty/schematics', collectionPath)

await check('collection.json registers ng-add, application, and library', () => {
	const collection = JSON.parse(readFileSync(collectionPath, 'utf-8'))
	assert.ok(collection.schematics['ng-add'], 'ng-add registered')
	assert.ok(collection.schematics['application'], 'application registered')
	assert.ok(collection.schematics['library'], 'library registered')
	assert.match(
		collection.schematics['ng-add'].factory,
		/ng-add\/index\.js#ngAdd/,
		'ng-add points at compiled factory',
	)
	assert.match(
		collection.schematics['application'].factory,
		/application\/index\.js#application/,
		'application points at compiled factory',
	)
	assert.match(
		collection.schematics['library'].factory,
		/library\/index\.js#library/,
		'library points at compiled factory',
	)
})

await check('ng-add rewrites builders + makes a host + scaffolds a sample remote', async () => {
	const tree = await runner.runSchematic('ng-add', {}, newWorkspaceTree())
	const ws = readJson(tree, '/angular.json')

	const shell = ws.projects.shell.architect
	assert.equal(shell.build.builder, '@treaty/build:application', 'build builder swapped to Treaty')
	assert.equal(shell.serve.builder, '@treaty/build:dev-server', 'serve builder swapped to Treaty')
	assert.equal(
		shell['extract-i18n'].builder,
		'@treaty/build:extract-i18n',
		'extract-i18n builder swapped to Treaty',
	)
	// existing options must be preserved, not clobbered.
	assert.equal(shell.build.options.browser, 'src/main.ts', 'build options preserved')

	// host federation config written at the project root.
	const fed = readJson(tree, '/federation.config.json')
	assert.equal(fed.name, 'shell', 'host federation name from project')
	assert.ok(fed.shared['@angular/core'], '@angular/core shared')
	assert.equal(fed.shared['@angular/core'].singleton, true, '@angular/core singleton')
	assert.equal(fed.shared['@angular/core'].eager, true, '@angular/core eager')
	assert.ok(fed.remotes['remote'], 'sample remote wired into host')
	assert.match(fed.remotes['remote'].entry, /remoteEntry\.js$/, 'remote entry url present')

	// sample remote scaffolded and registered on the Treaty builders.
	assert.ok(ws.projects.remote, 'sample remote project registered in angular.json')
	assert.equal(
		ws.projects.remote.architect.build.builder,
		'@treaty/build:application',
		'sample remote uses Treaty build builder',
	)
	const remoteMain = tree.read('/projects/remote/src/main.ts')
	assert.ok(remoteMain, 'sample remote main.ts scaffolded')
	assert.match(remoteMain.toString('utf-8'), /REMOTE/, 'remote main describes itself as a remote')
	const remoteFed = readJson(tree, '/projects/remote/federation.config.json')
	assert.ok(remoteFed.exposes['./Component'], 'sample remote exposes its component')
})

await check('ng-add --skipRemote converts builders only (no sample remote)', async () => {
	const tree = await runner.runSchematic('ng-add', { skipRemote: true }, newWorkspaceTree())
	const ws = readJson(tree, '/angular.json')
	assert.equal(
		ws.projects.shell.architect.build.builder,
		'@treaty/build:application',
		'build builder still swapped',
	)
	assert.equal(ws.projects.remote, undefined, 'no sample remote scaffolded')
	const fed = readJson(tree, '/federation.config.json')
	assert.deepEqual(fed.remotes, {}, 'host has no wired remotes when skipped')
})

await check('generate application (host) produces expected files + wiring', async () => {
	const tree = await runner.runSchematic('application', { name: 'dashboard' }, newWorkspaceTree())
	const ws = readJson(tree, '/angular.json')
	const proj = ws.projects.dashboard
	assert.ok(proj, 'application registered in angular.json')
	assert.equal(proj.projectType, 'application', 'projectType is application')
	assert.equal(proj.architect.build.builder, '@treaty/build:application', 'Treaty build builder')
	assert.equal(proj.architect.serve.builder, '@treaty/build:dev-server', 'Treaty serve builder')

	const base = '/projects/dashboard'
	for (const f of [
		`${base}/src/main.ts`,
		`${base}/src/index.html`,
		`${base}/src/app/app.component.ts`,
		`${base}/src/app/app.config.ts`,
		`${base}/src/app/app.routes.ts`,
		`${base}/tsconfig.app.json`,
		`${base}/federation.config.json`,
	]) {
		assert.ok(tree.exists(f), `expected ${f} to be generated`)
	}
	const comp = tree.read(`${base}/src/app/app.component.ts`).toString('utf-8')
	assert.match(comp, /selector: 'app-root'/, 'component selector uses prefix')
	assert.match(comp, /export class DashboardComponent/, 'component class classified from name')

	const fed = readJson(tree, `${base}/federation.config.json`)
	assert.equal(fed.name, 'dashboard', 'federation name dasherized from project name')
	assert.ok(fed.shared['@angular/core'], 'host shares Angular singletons')
	assert.deepEqual(fed.exposes, {}, 'host exposes nothing by default')

	const routes = tree.read(`${base}/src/app/app.routes.ts`).toString('utf-8')
	assert.match(routes, /HOST routes/, 'host routes template branch rendered')
})

await check('generate application --host=false produces an exposable remote', async () => {
	const tree = await runner.runSchematic(
		'application',
		{ name: 'widgets', host: false },
		newWorkspaceTree(),
	)
	const fed = readJson(tree, '/projects/widgets/federation.config.json')
	assert.ok(fed.exposes['./Component'], 'remote exposes its root component')
	assert.ok(fed.exposes['./routes'], 'remote exposes its routes')
	const main = tree.read('/projects/widgets/src/main.ts').toString('utf-8')
	assert.match(main, /REMOTE/, 'remote main describes itself as a remote')
})

await check('generate library produces an exposable remote library', async () => {
	const tree = await runner.runSchematic('library', { name: 'ui-kit' }, newWorkspaceTree())
	const ws = readJson(tree, '/angular.json')
	const proj = ws.projects['ui-kit']
	assert.ok(proj, 'library registered in angular.json')
	assert.equal(proj.projectType, 'library', 'projectType is library')
	assert.equal(proj.architect.build.builder, '@treaty/build:application', 'Treaty build builder')

	const base = '/projects/ui-kit'
	assert.ok(tree.exists(`${base}/src/public-api.ts`), 'public-api.ts generated')
	assert.ok(tree.exists(`${base}/src/lib/ui-kit.component.ts`), 'component file name interpolated')
	assert.ok(tree.exists(`${base}/federation.config.json`), 'federation config generated')

	const fed = readJson(tree, `${base}/federation.config.json`)
	assert.ok(fed.exposes['./Module'], 'library exposes its public API as ./Module')
	assert.ok(fed.shared['@angular/core'], 'library shares Angular singletons')

	const comp = tree.read(`${base}/src/lib/ui-kit.component.ts`).toString('utf-8')
	assert.match(comp, /export class UiKitComponent/, 'component class classified from name')
	assert.match(comp, /selector: 'lib-ui-kit'/, 'library prefix applied')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
