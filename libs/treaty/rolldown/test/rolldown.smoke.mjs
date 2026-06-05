// @ts-check
/**
 * Structural smoke test for `@treaty/rolldown` (no real Rolldown build required).
 *
 * Verifies the plugin factory returns the expected Rolldown plugins with the
 * Rollup-compatible hooks wired, that ownership/idempotency behave, and that the
 * partial-linker plugin delegates to the SHARED addon-backed `linkPartialCode`.
 *
 * Run: node test/rolldown.smoke.mjs
 */

import assert from 'node:assert/strict'
import { pathToFileURL } from 'node:url'
import { resolve } from 'node:path'

const distUrl = pathToFileURL(resolve(import.meta.dirname, '..', 'dist', 'index.js')).href
const mod = await import(distUrl)
const treaty = mod.default

// 1) Factory returns an array: [authoring plugin, partial-linker plugin].
const plugins = treaty()
assert.ok(Array.isArray(plugins), 'treaty() must return a Plugin[]')
assert.equal(plugins.length, 2, 'default: authoring + partial-linker')
const [authoring, linker] = plugins
assert.equal(authoring.name, 'rolldown-plugin-treaty')
assert.equal(linker.name, 'rolldown-plugin-treaty-link-partial')

// 2) The authoring plugin exposes only Rollup-compatible hooks (no Vite-only ones).
const expectedHooks = ['buildStart', 'resolveId', 'load', 'transform', 'watchChange', 'generateBundle']
for (const hook of expectedHooks) {
	assert.equal(typeof authoring[hook], 'function', `authoring plugin must implement ${hook}`)
}
for (const viteOnly of ['config', 'configResolved', 'configureServer', 'handleHotUpdate', 'enforce']) {
	assert.equal(authoring[viteOnly], undefined, `must NOT carry Vite-only ${viteOnly}`)
}

// 3) linkPartials: false drops the linker plugin.
assert.equal(treaty({ linkPartials: false }).length, 1, 'linkPartials:false => authoring only')

// 4) Ownership: a non-owned id is passed through (transform returns null), and an
//    already-lowered Ivy module is passed through (idempotency guard).
const ctx = {
	emitFile: () => 'ref',
	async resolve() {
		return null
	},
}
assert.equal(await authoring.transform.call(ctx, 'const x = 1', '/x.js'), null, 'non-owned .js passes through')
const loweredIvy = 'import * as i0 from "@angular/core";\nclass C {}\nC.ɵcmp = i0.ɵɵdefineComponent({});'
assert.equal(
	await authoring.transform.call(ctx, loweredIvy, '/c.treaty'),
	null,
	'already-lowered Ivy passes through (idempotency)',
)

// 5) resolveId: server-fn virtual ids resolve to themselves so `load` can serve them.
const virt = mod.SERVER_VIRTUAL_PREFIX + 'srvfn_abc'
assert.equal(await authoring.resolveId.call(ctx, virt, undefined, {}), virt, 'server virtual id self-resolves')

// 6) load: unknown id falls through to null (Rolldown's default loader).
assert.equal(authoring.load.call(ctx, '/some/file.ts'), null, 'unknown id => null (defer)')

// 7) The partial-linker plugin's transform bails on a non-partial module (returns
//    null / passes through) and is delegating to the shared addon helper.
assert.equal(typeof mod.linkPartialCode, 'function', 're-exports shared linkPartialCode')
assert.equal(typeof mod.isPartialModule, 'function', 're-exports shared isPartialModule')
assert.equal(
	linker.transform.call(ctx, 'export const a = 1', '/app/src/foo.ts'),
	null,
	'first-party module is not linked (passes through)',
)

console.log('ok: @treaty/rolldown structural smoke test passed')
