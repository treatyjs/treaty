/**
 * Unit test for @treaty/compiler server-function CHUNKING.
 *
 * The Rust authoring front-end returns ONE concatenated `serverModule` blob (a
 * runnable backend: shared preamble, every server fn declared verbatim, then a
 * `…/__server/<name>` route per fn, then a listen/router tail). This package
 * decomposes that blob into one chunk unit PER exported server fn so a bundler
 * can code-split each into a separately-loadable chunk.
 *
 * Asserts, for a component declaring TWO server fns:
 *   1. splitServerModule yields exactly two chunks,
 *   2. the chunks have DISTINCT, stable ids and DISTINCT client bindings,
 *   3. each chunk's `code` contains ONLY its own fn body (the other fn's body is
 *      absent), and every fn body is absent from the client bindings,
 *   4. ids are stable across calls and depend on file id + fn name (not body),
 *   5. buildServerFnManifest maps each id -> { exportName, chunkRef },
 *   6. a blob with no `…/__server/<name>` routes degrades to a single chunk,
 *   7. an empty blob yields no chunks.
 *
 * Runs against the TypeScript SOURCE directly (no tsc build), via the registered
 * `.js`->`.ts` resolver hook.
 *
 * Run:
 *   node --experimental-strip-types --import ./test/register-ts-source.mjs \
 *     test/server-chunks.spec.mjs
 */

import assert from 'node:assert/strict'
import {
	buildServerFnManifest,
	serverFnChunkId,
	splitServerModule,
} from '../src/server-chunks.ts'

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

const FILE_ID = 'src/dashboard.treaty'

// A two-server-fn backend blob in the shape the Express reference backend emits:
// a shared preamble, each author fn declared VERBATIM, then one `/__server/<name>`
// route per fn, then a listen tail. The unique tokens in each body let us prove a
// fn body never leaks into the other fn's chunk or into the client bindings.
const SAVE_BODY_TOKEN = 'PERSIST_TO_DB'
const LOAD_BODY_TOKEN = 'READ_FROM_DB'
const serverModule = [
	"const express = require('express');",
	'',
	'const app = express();',
	'app.use(express.json());',
	'',
	'async function save(record) {',
	`  return ${SAVE_BODY_TOKEN}(record);`,
	'}',
	'',
	'async function loadUser(id) {',
	`  return ${LOAD_BODY_TOKEN}(id);`,
	'}',
	'',
	"app.post('/__server/save', async (req, res) => {",
	'  const result = await save(req.body);',
	'  res.json(result);',
	'});',
	'',
	"app.post('/__server/loadUser', async (req, res) => {",
	'  const result = await loadUser(req.body);',
	'  res.json(result);',
	'});',
	'',
	'const PORT = process.env.PORT || 3000;',
	'app.listen(PORT, () => console.log(`server listening on ${PORT}`));',
	'',
].join('\n')

const chunks = splitServerModule(FILE_ID, serverModule)

// 1. two server fns -> two chunks, named for the author's exports.
check('two server fns yield two chunks', () => {
	assert.equal(chunks.length, 2, `expected 2 chunks, got ${chunks.length}`)
	const names = chunks.map((c) => c.exportName).sort()
	assert.deepEqual(names, ['loadUser', 'save'], `unexpected export names: ${names}`)
})

// 2. distinct, stable ids and distinct client bindings.
check('chunks have distinct ids and distinct client bindings', () => {
	const [a, b] = chunks
	assert.notEqual(a.id, b.id, 'chunk ids must be distinct')
	assert.notEqual(a.clientBinding, b.clientBinding, 'client bindings must be distinct')
	// Each client binding references its own chunk id and re-exports its own name.
	for (const c of chunks) {
		assert.ok(c.clientBinding.includes(c.id), `client binding must reference chunk id ${c.id}`)
		assert.ok(
			c.clientBinding.includes(`export { ${c.exportName} }`),
			`client binding must re-export ${c.exportName}`
		)
	}
})

// 3. each chunk carries ONLY its own fn body; bodies never enter client bindings.
check('fn bodies are isolated per chunk and absent from client bindings', () => {
	const save = chunks.find((c) => c.exportName === 'save')
	const load = chunks.find((c) => c.exportName === 'loadUser')
	assert.ok(save && load, 'both chunks present')

	// save chunk has the save body, NOT the loadUser body.
	assert.ok(save.code.includes(SAVE_BODY_TOKEN), 'save chunk must contain its own body')
	assert.ok(
		!save.code.includes(LOAD_BODY_TOKEN),
		'save chunk must NOT contain the loadUser body'
	)
	// loadUser chunk has the loadUser body, NOT the save body.
	assert.ok(load.code.includes(LOAD_BODY_TOKEN), 'loadUser chunk must contain its own body')
	assert.ok(
		!load.code.includes(SAVE_BODY_TOKEN),
		'loadUser chunk must NOT contain the save body'
	)
	// The fn BODY must never appear in any client binding (client/Ivy side).
	for (const c of chunks) {
		assert.ok(
			!c.clientBinding.includes(SAVE_BODY_TOKEN) &&
				!c.clientBinding.includes(LOAD_BODY_TOKEN),
			'no fn body may leak into a client binding'
		)
	}
	// Both chunks still carry the shared preamble (so each is independently runnable).
	for (const c of chunks) {
		assert.ok(c.code.includes("require('express')"), 'each chunk keeps the shared preamble')
	}
})

// 4. ids are stable + derived from file id + fn name (not the body).
check('chunk ids are stable and body-independent', () => {
	const save = chunks.find((c) => c.exportName === 'save')
	assert.equal(save.id, serverFnChunkId(FILE_ID, 'save'), 'id must equal serverFnChunkId()')
	// Re-splitting the same input is deterministic.
	const again = splitServerModule(FILE_ID, serverModule)
	assert.deepEqual(
		again.map((c) => c.id),
		chunks.map((c) => c.id),
		'ids must be stable across calls'
	)
	// Same file + fn but a DIFFERENT body still hashes to the same id.
	const otherBody = serverModule.replace(SAVE_BODY_TOKEN, 'DIFFERENT_IMPL')
	const otherChunks = splitServerModule(FILE_ID, otherBody)
	const otherSave = otherChunks.find((c) => c.exportName === 'save')
	assert.equal(otherSave.id, save.id, 'id must not depend on the fn body')
	// A different file id gives a different id for the same fn name.
	assert.notEqual(serverFnChunkId('src/other.treaty', 'save'), save.id, 'id must depend on file')
})

// 5. buildServerFnManifest maps id -> { exportName, chunkRef } across results.
check('buildServerFnManifest maps fn id -> exportName + chunkRef', () => {
	const manifest = buildServerFnManifest([{ code: '', sideEffects: false, serverChunks: chunks }])
	const keys = Object.keys(manifest)
	assert.equal(keys.length, 2, 'manifest has one entry per chunk')
	for (const c of chunks) {
		const entry = manifest[c.id]
		assert.ok(entry, `manifest must contain ${c.id}`)
		assert.equal(entry.exportName, c.exportName, 'manifest export name matches chunk')
		assert.equal(entry.chunkRef, c.id, 'manifest chunkRef is the stable chunk id')
	}
	// Results without serverChunks contribute nothing; null/undefined are ignored.
	const empty = buildServerFnManifest([null, undefined, { code: '', sideEffects: false }])
	assert.equal(Object.keys(empty).length, 0, 'no chunks -> empty manifest')
})

// 6. a blob with no `/__server/<name>` routes degrades to a single whole-blob chunk.
check('no discoverable routes degrades to a single chunk', () => {
	const opaque = 'fn main() { println!("no routes here"); }\n'
	const one = splitServerModule(FILE_ID, opaque)
	assert.equal(one.length, 1, 'opaque blob -> single chunk')
	assert.equal(one[0].code, opaque, 'single chunk preserves the whole blob')
	assert.ok(one[0].clientBinding.includes(one[0].id), 'single chunk has a client binding')
})

// 7. an empty blob yields no chunks.
check('empty server module yields no chunks', () => {
	assert.deepEqual(splitServerModule(FILE_ID, ''), [], 'empty blob -> no chunks')
	assert.deepEqual(splitServerModule(FILE_ID, '   \n\t'), [], 'whitespace blob -> no chunks')
})

// 8. the default axum (Rust) backend shape also chunks per fn with body isolation.
//    Its handlers are named `__server_<name>` with a `<Pascal>Request` struct and
//    a shared `build_router()`; the split must still isolate each fn's body.
check('axum backend shape chunks per fn with isolated bodies', () => {
	const axumModule = [
		'use axum::{Json, Router, routing::post};',
		'',
		'#[derive(Debug, Deserialize)]',
		'pub struct SaveRequest { pub record: String }',
		'pub async fn __server_save(Json(req): Json<SaveRequest>) -> Json<String> {',
		`    Json(${SAVE_BODY_TOKEN}(req.record))`,
		'}',
		'#[derive(Debug, Deserialize)]',
		'pub struct LoadUserRequest { pub id: String }',
		'pub async fn __server_loadUser(Json(req): Json<LoadUserRequest>) -> Json<String> {',
		`    Json(${LOAD_BODY_TOKEN}(req.id))`,
		'}',
		'pub fn build_router() -> Router {',
		'    Router::new()',
		'        .route("/__server/save", post(__server_save))',
		'        .route("/__server/loadUser", post(__server_loadUser))',
		'}',
		'',
	].join('\n')
	const axumChunks = splitServerModule('src/api.treaty', axumModule)
	assert.equal(axumChunks.length, 2, `expected 2 axum chunks, got ${axumChunks.length}`)
	const save = axumChunks.find((c) => c.exportName === 'save')
	const load = axumChunks.find((c) => c.exportName === 'loadUser')
	assert.ok(save && load, 'both axum chunks present')
	assert.ok(save.code.includes(SAVE_BODY_TOKEN) && !save.code.includes(LOAD_BODY_TOKEN), 'save body isolated')
	assert.ok(load.code.includes(LOAD_BODY_TOKEN) && !load.code.includes(SAVE_BODY_TOKEN), 'loadUser body isolated')
	assert.notEqual(save.id, load.id, 'axum chunk ids distinct')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSERVER-CHUNKS TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSERVER-CHUNKS TEST PASSED')
