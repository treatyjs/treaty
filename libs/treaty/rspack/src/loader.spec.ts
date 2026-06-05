/**
 * Smoke specs for the Treaty Rspack loader. These drive {@link treatyLoader}
 * with a hand-rolled, structurally-typed loader context (`this`) — no real
 * Rspack — and assert the emitted Ivy JS for a `.treaty` source, plus the
 * pass-through and error-reporting behaviours.
 */

import { describe, expect, it } from 'bun:test'
import { treatyLoader, type TreatyLoaderContext } from './loader.ts'
import { TreatyRspackPlugin, treatyRule } from './plugin.ts'
import type { TreatyLoaderOptions } from './options.ts'

const TREATY_SOURCE = `<script>
class Hello {
  name = 'world'
}
</script>
<template>
  <h1>Hello {{ name }}</h1>
</template>`

/**
 * A `.ts` `@Component` source. The base-Angular front-end emits a v3 source map
 * for this shape, so it is the deterministic fixture for the map-forwarding case
 * (the `.treaty`/JSX front-ends do not yet produce a map).
 */
const TS_COMPONENT_SOURCE =
	"import { Component } from '@angular/core'\n" +
	"@Component({ selector: 'app-x', template: '<div>x</div>' })\n" +
	'export class XComponent {}\n'

/** Build a fake loader context capturing the result the loader reports. */
function fakeContext(
	resourcePath: string,
	options: TreatyLoaderOptions = {}
): {
	ctx: TreatyLoaderContext
	result: () => { error: Error | null | undefined; content?: string; map?: unknown }
} {
	let captured: { error: Error | null | undefined; content?: string; map?: unknown } = {
		error: undefined,
	}
	const ctx: TreatyLoaderContext = {
		resourcePath,
		resource: resourcePath,
		getOptions: () => options,
		async: () => () => {
			throw new Error('async path not expected in this spec')
		},
		callback: (error, content, map) => {
			captured = { error, content, map }
		},
	}
	return { ctx, result: () => captured }
}

describe('treatyLoader', () => {
	it('lowers a .treaty source to Ivy JS', () => {
		const { ctx, result } = fakeContext('/abs/Hello.treaty')
		const ret = treatyLoader.call(ctx, TREATY_SOURCE)

		// The loader reports through this.callback, so it returns nothing.
		expect(ret).toBeUndefined()

		const { error, content } = result()
		expect(error).toBeNull()
		expect(typeof content).toBe('string')
		expect(content).toContain('ɵɵdefineComponent')
		expect(content).toContain('@angular/core')
		expect(content).toContain('ɵfac')
		// It is Ivy JS, not the original SFC markup.
		expect(content).not.toContain('<template>')
	})

	it('returns Ivy JS directly when no callback is present on the context', () => {
		const ctx: TreatyLoaderContext = {
			resourcePath: '/abs/Direct.treaty',
			getOptions: () => ({}),
			async: () => () => undefined,
		}
		const out = treatyLoader.call(ctx, TREATY_SOURCE)
		expect(typeof out).toBe('string')
		expect(out as string).toContain('ɵɵdefineComponent')
	})

	it('forwards the v3 source map through the loader callback', () => {
		const { ctx, result } = fakeContext('/abs/X.ts')
		const ret = treatyLoader.call(ctx, TS_COMPONENT_SOURCE)

		// Delivered through this.callback, so the loader returns nothing.
		expect(ret).toBeUndefined()

		const { error, content, map } = result()
		expect(error).toBeNull()
		expect(typeof content).toBe('string')
		expect(content).toContain('ɵɵdefineComponent')

		// The compiler's serialized JSON map is parsed to the object shape
		// webpack/rspack's callback expects, and forwarded as the third arg.
		expect(map).toBeDefined()
		const parsed = map as { version?: number; mappings?: string }
		expect(parsed.version).toBe(3)
		expect(typeof parsed.mappings).toBe('string')
	})

	it('passes through modules it does not own', () => {
		const plain = 'export const answer = 42\n'
		const { ctx, result } = fakeContext('/abs/util.ts')
		const ret = treatyLoader.call(ctx, plain)

		// A plain .ts without @Component is not Treaty's; source is returned verbatim.
		expect(ret).toBe(plain)
		expect(result().error).toBeUndefined()
	})

	it('reports compiler errors through the loader callback', () => {
		const { ctx, result } = fakeContext('/abs/Broken.treaty')
		// An unterminated <script> block is a real compiler diagnostic (the parser
		// hits EOF mid-expression), which the loader must surface as an Error.
		const broken = '<script>const x = (\n<template><div></div></template>'
		const ret = treatyLoader.call(ctx, broken)
		expect(ret).toBeUndefined()
		const { error } = result()
		expect(error).toBeInstanceOf(Error)
	})
})

describe('TreatyRspackPlugin', () => {
	it('registers the loader rule and resolve extensions', () => {
		const host = { options: {} as Record<string, unknown> }
		new TreatyRspackPlugin().apply(host as never)

		const opts = host.options as {
			module: { rules: Array<{ test: RegExp; use: Array<{ loader: string }> }> }
			resolve: { extensions: string[] }
		}

		expect(opts.module.rules).toHaveLength(1)
		const rule = opts.module.rules[0]!
		expect(rule.test.test('Foo.treaty')).toBe(true)
		expect(rule.test.test('Foo.tsx')).toBe(true)
		expect(rule.test.test('Foo.tjsx')).toBe(true)
		expect(rule.test.test('Foo.css')).toBe(false)
		expect(rule.use[0]!.loader).toContain('loader')

		expect(opts.resolve.extensions).toEqual(['.treaty', '.tsx', '.tjsx'])
	})

	it('does not duplicate pre-existing resolve extensions', () => {
		const host = {
			options: { resolve: { extensions: ['.treaty', '.mjs'] } } as Record<
				string,
				unknown
			>,
		}
		new TreatyRspackPlugin().apply(host as never)
		const exts = (host.options as { resolve: { extensions: string[] } }).resolve
			.extensions
		expect(exts.filter((e) => e === '.treaty')).toHaveLength(1)
		expect(exts).toContain('.mjs')
		expect(exts).toContain('.tsx')
	})

	it('treatyRule honours a custom test and forwards compiler options', () => {
		const custom = /\.treaty$/
		const rule = treatyRule({ test: custom, cache: false })
		expect(rule.test).toBe(custom)
		expect(rule.use![0]!.options).toEqual({ cache: false })
	})
})
