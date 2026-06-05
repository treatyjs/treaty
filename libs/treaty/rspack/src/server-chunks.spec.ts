/**
 * Smoke specs for Treaty server-function CHUNKING in the Rspack integration.
 *
 * `@treaty/compiler` hands a transform result its server fns as INDIVIDUAL
 * {@link ServerFnChunk} units. These specs drive the Rspack-side wiring directly
 * with synthetic transform results (so no Rust addon round trip is needed) and a
 * hand-rolled, structurally-typed Rspack surface, asserting:
 *   1. each server fn is emitted as its OWN loadable chunk asset (one file per fn),
 *   2. the per-fn body lands ONLY in its chunk asset — never in the client module,
 *   3. the client module carries only the lazy per-fn bindings (a code-split
 *      `import()` boundary), and the binding/asset are named by the stable fn id,
 *   4. the plugin emits the aggregate fn-id -> chunk manifest as a build asset,
 *   5. a result with no server chunks passes the client code through untouched,
 *   6. the per-compilation registry is shared loader<->plugin and de-dupes by id.
 */

// `_compilation` is Rspack/webpack's own loader-context property (its API name).
/* oxlint-disable no-underscore-dangle */

import { describe, expect, it } from 'bun:test'
import type { ServerFnChunk, TransformResult } from '@treaty/compiler'
import { treatyLoader, type TreatyLoaderContext } from './loader.ts'
import { TreatyRspackPlugin, type TreatyCompilation } from './plugin.ts'
import {
	emitServerFnChunks,
	registryFor,
	SERVER_FN_MANIFEST_ASSET,
	serverFnAssetName,
	TreatyServerFnRegistry,
	type RspackServerFnManifest,
} from './server-chunks.ts'

const SAVE_BODY = 'PERSIST_TO_DB'
const LOAD_BODY = 'READ_FROM_DB'

/** Two synthetic server-fn chunks in the shape `@treaty/compiler` produces. */
function twoChunks(): ServerFnChunk[] {
	return [
		{
			id: 'srvfn_aaa111',
			exportName: 'save',
			code: `async function save(r) { return ${SAVE_BODY}(r); }\napp.post('/__server/save', save);`,
			clientBinding: "import { save } from './srvfn_aaa111.server.js';\nexport { save };",
		},
		{
			id: 'srvfn_bbb222',
			exportName: 'loadUser',
			code: `async function loadUser(id) { return ${LOAD_BODY}(id); }\napp.post('/__server/loadUser', loadUser);`,
			clientBinding:
				"import { loadUser } from './srvfn_bbb222.server.js';\nexport { loadUser };",
		},
	]
}

/** A transform result carrying server chunks (a body-free client `code`). */
function resultWithChunks(chunks: ServerFnChunk[]): TransformResult {
	return {
		code: "export class Cmp {}\nexport const ɵfac = () => new Cmp();\n",
		sideEffects: false,
		serverModule: chunks.map((c) => c.code).join('\n\n'),
		serverChunks: chunks,
	}
}

/** A minimal emitter capturing every emitted asset name -> content. */
function fakeEmitter(): {
	emitFile: (name: string, content: string) => void
	assets: Map<string, string>
} {
	const assets = new Map<string, string>()
	return { emitFile: (name, content) => assets.set(name, content), assets }
}

describe('emitServerFnChunks', () => {
	it('emits one loadable chunk asset per server fn, body-isolated', () => {
		const chunks = twoChunks()
		const emitter = fakeEmitter()
		const registry = new TreatyServerFnRegistry()
		emitServerFnChunks(emitter, registry, resultWithChunks(chunks))

		// One asset per fn, named by the stable fn id.
		expect(emitter.assets.size).toBe(2)
		const saveAsset = emitter.assets.get('srvfn_aaa111.server.js')
		const loadAsset = emitter.assets.get('srvfn_bbb222.server.js')
		expect(saveAsset).toBeDefined()
		expect(loadAsset).toBeDefined()

		// Each chunk asset holds ONLY its own fn body.
		expect(saveAsset).toContain(SAVE_BODY)
		expect(saveAsset).not.toContain(LOAD_BODY)
		expect(loadAsset).toContain(LOAD_BODY)
		expect(loadAsset).not.toContain(SAVE_BODY)
	})

	it('returns a client module with only lazy bindings — never the fn body', () => {
		const chunks = twoChunks()
		const emitter = fakeEmitter()
		const registry = new TreatyServerFnRegistry()
		const client = emitServerFnChunks(emitter, registry, resultWithChunks(chunks))

		// The client (Ivy) module keeps its component code...
		expect(client).toContain('export class Cmp')
		// ...exposes each fn as a lazy, code-split import() boundary keyed by fn id...
		for (const c of chunks) {
			expect(client).toContain(`import(/* webpackChunkName: ${JSON.stringify(c.id)} */`)
			expect(client).toContain(`./${serverFnAssetName(c)}`)
			expect(client).toContain(`export const ${c.exportName} =`)
		}
		// ...and NO server fn body ever reaches the client module.
		expect(client).not.toContain(SAVE_BODY)
		expect(client).not.toContain(LOAD_BODY)
	})

	it('passes the client code through untouched when there are no server chunks', () => {
		const emitter = fakeEmitter()
		const registry = new TreatyServerFnRegistry()
		const plain: TransformResult = { code: 'export const x = 1\n', sideEffects: false }
		const out = emitServerFnChunks(emitter, registry, plain)
		expect(out).toBe(plain.code)
		expect(emitter.assets.size).toBe(0)
		expect(registry.isEmpty).toBe(true)
	})
})

describe('TreatyServerFnRegistry', () => {
	it('records chunks and builds a fn-id -> chunk manifest', () => {
		const chunks = twoChunks()
		const registry = new TreatyServerFnRegistry()
		registry.add(resultWithChunks(chunks))

		const manifest = registry.manifest()
		expect(Object.keys(manifest).sort()).toEqual(['srvfn_aaa111', 'srvfn_bbb222'])
		const save = manifest['srvfn_aaa111']!
		expect(save.exportName).toBe('save')
		expect(save.chunkRef).toBe('srvfn_aaa111')
		expect(save.asset).toBe('srvfn_aaa111.server.js')
	})

	it('de-dupes by stable chunk id across repeated adds', () => {
		const chunks = twoChunks()
		const registry = new TreatyServerFnRegistry()
		registry.add(resultWithChunks(chunks))
		registry.add(resultWithChunks(chunks))
		expect(registry.all()).toHaveLength(2)
	})
})

describe('treatyLoader server-fn emission', () => {
	it('emits per-fn chunk assets and a body-free client module through the loader', () => {
		const chunks = twoChunks()
		const emitter = fakeEmitter()
		const compilation = {}
		let captured = ''
		const ctx: TreatyLoaderContext = {
			resourcePath: '/abs/Dash.treaty',
			resource: '/abs/Dash.treaty',
			getOptions: () => ({}),
			async: () => () => undefined,
			callback: (_e, content) => {
				captured = content ?? ''
			},
			emitFile: emitter.emitFile,
			_compilation: compilation,
		}

		// Exercise the loader's emission path directly with the synthetic result, so
		// no Rust addon round trip is required (the result stands in for what the
		// compiler would hand the loader for a server-fn-bearing file).
		const client = emitServerFnChunks(
			{ emitFile: ctx.emitFile!.bind(ctx) },
			registryFor(ctx._compilation),
			resultWithChunks(chunks)
		)
		ctx.callback!(null, client)

		expect(captured).toContain('export const save =')
		expect(captured).not.toContain(SAVE_BODY)
		expect(emitter.assets.has('srvfn_aaa111.server.js')).toBe(true)
		expect(emitter.assets.has('srvfn_bbb222.server.js')).toBe(true)

		// The registry the loader fed (keyed by this compilation) carries both fns —
		// the same instance the plugin reads to emit the manifest.
		expect(registryFor(compilation).all()).toHaveLength(2)
	})

	it('still lowers a plain .treaty SFC (no server fns) to body-free Ivy JS', () => {
		// Sanity: a server-fn-free file flows through the existing loader path and
		// never emits a chunk asset.
		const emitter = fakeEmitter()
		let captured = ''
		const ctx: TreatyLoaderContext = {
			resourcePath: '/abs/Plain.treaty',
			resource: '/abs/Plain.treaty',
			getOptions: () => ({}),
			async: () => () => undefined,
			callback: (_e, content) => {
				captured = content ?? ''
			},
			emitFile: emitter.emitFile,
			_compilation: {},
		}
		treatyLoader.call(
			ctx,
			"<script>class Plain { n = 1 }</script><template>{{ n }}</template>"
		)
		expect(captured).toContain('ɵɵdefineComponent')
		expect(emitter.assets.size).toBe(0)
	})
})

describe('TreatyRspackPlugin server-fn manifest', () => {
	/** A fake compiler+compilation surface capturing the emitted manifest asset. */
	function fakeCompiler(): {
		host: Parameters<TreatyRspackPlugin['apply']>[0]
		run: () => void
		assets: Map<string, string>
		compilation: object
	} {
		const assets = new Map<string, string>()
		let processAssetsFn: (() => void) | undefined
		const compilation: TreatyCompilation & object = {
			hooks: {
				processAssets: {
					tap: (_o, fn) => {
						processAssetsFn = fn
					},
				},
			},
			emitAsset: (name, source) => {
				assets.set(name, String((source as { source?: () => string }).source?.() ?? source))
			},
		}
		let thisCompilationFn: ((c: TreatyCompilation) => void) | undefined
		const host = {
			options: {} as Record<string, unknown>,
			hooks: {
				thisCompilation: {
					tap: (_n: string, fn: (c: TreatyCompilation) => void) => {
						thisCompilationFn = fn
					},
				},
			},
			webpack: {
				sources: {
					RawSource: class {
						constructor(private readonly value: string) {}
						source(): string {
							return this.value
						}
					},
				},
			},
		}
		return {
			host: host as never,
			compilation,
			assets,
			run: () => {
				thisCompilationFn?.(compilation)
				processAssetsFn?.()
			},
		}
	}

	it('emits the fn-id -> chunk manifest asset when the loader recorded chunks', () => {
		const { host, run, assets, compilation } = fakeCompiler()
		// Disable auto-MF so the (absent) federation peer is not required here.
		new TreatyRspackPlugin({ moduleFederation: false }).apply(host)

		// Feed the SAME per-compilation registry the plugin will read.
		registryFor(compilation).add(resultWithChunks(twoChunks()))
		run()

		const json = assets.get(SERVER_FN_MANIFEST_ASSET)
		expect(json).toBeDefined()
		const manifest = JSON.parse(json!) as RspackServerFnManifest
		expect(Object.keys(manifest).sort()).toEqual(['srvfn_aaa111', 'srvfn_bbb222'])
		expect(manifest['srvfn_aaa111']!.exportName).toBe('save')
		expect(manifest['srvfn_aaa111']!.asset).toBe('srvfn_aaa111.server.js')
		// The manifest carries only references — no fn body leaks into it.
		expect(json).not.toContain(SAVE_BODY)
		expect(json).not.toContain(LOAD_BODY)
	})

	it('emits no manifest asset when no server fns were recorded', () => {
		const { host, run, assets } = fakeCompiler()
		new TreatyRspackPlugin({ moduleFederation: false }).apply(host)
		run()
		expect(assets.has(SERVER_FN_MANIFEST_ASSET)).toBe(false)
	})
})
