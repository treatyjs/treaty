/**
 * Unit + end-to-end test for @treaty/compiler SOURCE-MAP threading and the
 * CLIENT-PRIVACY guarantee for extracted server-fn bodies.
 *
 * The Rust authoring addon returns an additive Source Map v3 JSON (`map`) on the
 * compiled result, already server-body-redacted: when the source declared a
 * `server { … }` block, every lifted server-fn body has been blanked out of the
 * map's `sourcesContent` before the map reaches this package. This package threads
 * that `map` onto {@link TransformResult.map} in `postProcess`, leaving
 * `serverChunks`/`serverModule` behaviour unchanged.
 *
 * Asserts:
 *   1. assertNoServerBodyInMap passes a clean result and flags a leaked one
 *      (the defensive privacy helper used in tests);
 *   2. assertNoServerBodyInMap short-circuits to ok when there is no map or no
 *      serverChunks (nothing to leak);
 *   3. isValidSourceMapV3 accepts a real v3 map and rejects malformed input;
 *   4. END-TO-END: a `.ts` @Component carrying an inline `server { … }` block
 *      transforms to a result whose `.map` is valid v3 AND whose `sources` /
 *      `sourcesContent` do NOT contain the server-fn body token, while the
 *      server body still lives in `serverModule` / `serverChunks` (unchanged);
 *   5. a plain component (no server block) still threads its map through.
 *
 * The helper cases run against the TypeScript SOURCE directly via the registered
 * `.js`->`.ts` resolver hook; the end-to-end case drives the BUILT TreatyCompiler
 * (dist) so it exercises the real addon round trip.
 *
 * Run:
 *   node --experimental-strip-types --import ./test/register-ts-source.mjs \
 *     test/source-map.spec.mjs
 */

import assert from 'node:assert/strict'
import {
	assertNoServerBodyInMap,
	isValidSourceMapV3,
} from '../src/server-chunks.ts'
import { TreatyCompiler } from '../dist/index.js'

let failures = 0
const results = []

function check(label, fn) {
	try {
		fn()
		results.push(`PASS ${label}`)
	} catch (err) {
		failures++
		results.push(`FAIL ${label}: ${err.message}`)
	}
}

// A distinctive token that must never reach the client map.
const SERVER_BODY_TOKEN = 'SECRET_DB_PERSIST'

// A server-fn chunk in the default axum backend shape, carrying the body token.
const serverChunk = {
	id: 'srvfn_test',
	exportName: 'save',
	code: [
		'use axum::{Json, Router, routing::post};',
		'pub async fn __server_save(Json(req): Json<SaveRequest>) -> Json<Value> {',
		'    let user = req.user;',
		`    return ${SERVER_BODY_TOKEN}(user);`,
		'}',
		'    .route("/__server/save", post(__server_save))',
	].join('\n'),
	clientBinding: 'import { save } from "./srvfn_test.server.js";\nexport { save };',
}

/** Build a Source Map v3 JSON whose sourcesContent is exactly `content`. */
function mapWithContent(content) {
	return JSON.stringify({
		version: 3,
		file: 'out.js',
		sources: ['form.ts'],
		sourcesContent: [content],
		names: [],
		mappings: 'AAAA',
	})
}

// 1. helper flags a leaked body and passes a redacted one.
check('assertNoServerBodyInMap flags a leaked server body and passes a clean map', () => {
	// A map whose sourcesContent still carries the body token -> LEAK.
	const leaky = {
		map: mapWithContent(`function comp(){}\nreturn ${SERVER_BODY_TOKEN}(user);\n`),
		serverChunks: [serverChunk],
	}
	const leak = assertNoServerBodyInMap(leaky)
	assert.equal(leak.ok, false, 'a body token in sourcesContent must be reported as a leak')
	assert.ok(leak.leak, 'leak detail present')
	assert.equal(leak.leak.where, 'sourcesContent', 'leak located in sourcesContent')
	assert.ok(leak.leak.token.includes(SERVER_BODY_TOKEN), 'reported token is the leaked body line')

	// A map whose body has been blanked (position-preserving) -> CLEAN.
	const clean = {
		map: mapWithContent('function comp(){}\n                            \n'),
		serverChunks: [serverChunk],
	}
	assert.equal(assertNoServerBodyInMap(clean).ok, true, 'a redacted map must pass')

	// A leak in `sources` (not just content) is also caught.
	const inSources = {
		map: JSON.stringify({
			version: 3,
			sources: [`form.ts?inline=return ${SERVER_BODY_TOKEN}(user);`],
			sourcesContent: ['function comp(){}'],
			names: [],
			mappings: 'AAAA',
		}),
		serverChunks: [serverChunk],
	}
	const srcLeak = assertNoServerBodyInMap(inSources)
	assert.equal(srcLeak.ok, false, 'a body token in sources must be reported as a leak')
	assert.equal(srcLeak.leak.where, 'sources', 'leak located in sources')
})

// 2. nothing to leak short-circuits to ok.
check('assertNoServerBodyInMap is ok when there is no map or no serverChunks', () => {
	assert.equal(
		assertNoServerBodyInMap({ serverChunks: [serverChunk] }).ok,
		true,
		'no map -> nothing can leak'
	)
	assert.equal(
		assertNoServerBodyInMap({ map: mapWithContent(`x ${SERVER_BODY_TOKEN}`) }).ok,
		true,
		'no serverChunks -> no server bodies exist to check'
	)
	assert.equal(
		assertNoServerBodyInMap({ map: mapWithContent('clean'), serverChunks: [] }).ok,
		true,
		'empty serverChunks -> ok'
	)
})

// 3. v3 validity check.
check('isValidSourceMapV3 accepts a real v3 map and rejects malformed input', () => {
	assert.equal(isValidSourceMapV3(mapWithContent('x')), true, 'well-formed v3 map accepted')
	assert.equal(isValidSourceMapV3('not json'), false, 'non-JSON rejected')
	assert.equal(isValidSourceMapV3(JSON.stringify({ version: 2 })), false, 'wrong version rejected')
	assert.equal(
		isValidSourceMapV3(JSON.stringify({ version: 3, sources: [] })),
		false,
		'missing mappings rejected'
	)
	assert.equal(
		isValidSourceMapV3(JSON.stringify({ version: 3, mappings: 'AAAA' })),
		false,
		'missing sources array rejected'
	)
})

const compiler = new TreatyCompiler()

// 4. END-TO-END: a component with an inline server fn -> valid v3 map whose
//    sourcesContent does NOT contain the server body token; server behaviour
//    (serverModule + serverChunks) is unchanged.
check('inline server-fn component: map is valid v3 and carries no server body', () => {
	const source = [
		"import { Component } from '@angular/core';",
		'server {',
		`  async function save(user) { return ${SERVER_BODY_TOKEN}(user); }`,
		'}',
		"@Component({ selector: 'app-form', template: '<div>{{title}}</div>' })",
		'export class FormComponent {',
		"  title = 'x';",
		'  onClick(user) { return save(user); }',
		'}',
		'',
	].join('\n')

	const out = compiler.transform('form.ts', source)
	assert.ok(out, 'expected a non-null transform result')

	// Server behaviour unchanged: the body lives in the server module / chunks.
	assert.ok(out.serverModule, 'a server { … } block must yield a serverModule')
	assert.ok(
		out.serverModule.includes(SERVER_BODY_TOKEN),
		'the server body must remain in the (server-side) serverModule'
	)
	assert.ok(out.serverChunks && out.serverChunks.length > 0, 'serverChunks derived from the blob')
	assert.ok(
		out.serverChunks.some((c) => c.code.includes(SERVER_BODY_TOKEN)),
		'a server chunk carries the body (server side only)'
	)

	// The client code never carries the body.
	assert.ok(!out.code.includes(SERVER_BODY_TOKEN), 'server body must NOT reach the client code')

	// The map is threaded through, is valid v3, and is body-redacted.
	assert.ok(out.map !== undefined, 'the addon map must be threaded onto TransformResult.map')
	assert.ok(isValidSourceMapV3(out.map), 'the threaded map must be valid Source Map v3')

	const v3 = JSON.parse(out.map)
	const sourcesContent = (v3.sourcesContent ?? []).join('\n')
	assert.ok(
		!sourcesContent.includes(SERVER_BODY_TOKEN),
		'CLIENT PRIVACY: server body token must be absent from sourcesContent'
	)
	assert.ok(
		!(v3.sources ?? []).join('\n').includes(SERVER_BODY_TOKEN),
		'CLIENT PRIVACY: server body token must be absent from sources'
	)

	// The defensive helper agrees: no body leaks into the map.
	const audit = assertNoServerBodyInMap(out)
	assert.equal(audit.ok, true, `privacy audit must pass; leak=${JSON.stringify(audit.leak)}`)

	results.push(`INFO server component emitted a ${out.map.length}-byte redacted map`)
})

// 5. a plain component (no server block) still threads its map through.
check('plain component threads a valid v3 map with no serverChunks', () => {
	const source =
		"import { Component } from '@angular/core';\n" +
		"@Component({ selector: 'app-plain', template: '<div>{{title}}</div>' })\n" +
		"export class PlainComponent { title = 'hi'; }\n"
	const out = compiler.transform('plain.ts', source)
	assert.ok(out, 'expected a result')
	assert.equal(out.serverModule, undefined, 'no server block -> no serverModule')
	assert.equal(out.serverChunks, undefined, 'no server block -> no serverChunks')
	assert.ok(out.map !== undefined, 'map must still be threaded for a plain component')
	assert.ok(isValidSourceMapV3(out.map), 'threaded map is valid v3')
	// With no server chunks, the privacy audit is vacuously ok.
	assert.equal(assertNoServerBodyInMap(out).ok, true, 'no chunks -> audit ok')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSOURCE-MAP TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSOURCE-MAP TEST PASSED')
