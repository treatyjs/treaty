/**
 * Smoke specs for the Treaty Rsbuild loader (the `tools.rspack` fallback path).
 * They drive {@link treatyLoader} with a hand-rolled, structurally-typed loader
 * context (no real Rspack) and assert that when the core produces a v3 source
 * map the loader forwards it through the webpack/rspack `this.callback(err, code,
 * map)` contract — the only loader path that can carry a map — and otherwise
 * returns the emitted code directly.
 */

import { describe, expect, it } from 'bun:test'
import treatyLoader from './loader.ts'
import type { TreatyPluginOptions } from './options.ts'

/** A `.ts` `@Component` source: the base-Angular front-end emits a v3 map for it. */
const TS_COMPONENT_SOURCE =
	"import { Component } from '@angular/core'\n" +
	"@Component({ selector: 'app-x', template: '<div>x</div>' })\n" +
	'export class XComponent {}\n'

/** A `.treaty` source — owned by the compiler but the front-end emits no map yet. */
const TREATY_SOURCE = `<script>
class Hello { name = 'world' }
</script>
<template><h1>Hello {{ name }}</h1></template>`

/** The loader `this` context the loader relies on, capturing the callback result. */
interface FakeLoaderContext {
	resourcePath: string
	getOptions: () => TreatyPluginOptions
	callback?: (error: Error | null | undefined, content?: string, map?: unknown) => void
}

function fakeContext(
	resourcePath: string,
	options: TreatyPluginOptions = {}
): {
	ctx: FakeLoaderContext
	result: () => { error: Error | null | undefined; content?: string; map?: unknown }
} {
	let captured: { error: Error | null | undefined; content?: string; map?: unknown } = {
		error: undefined,
	}
	const ctx: FakeLoaderContext = {
		resourcePath,
		getOptions: () => options,
		callback: (error, content, map) => {
			captured = { error, content, map }
		},
	}
	return { ctx, result: () => captured }
}

describe('treatyLoader (rsbuild fallback)', () => {
	it('forwards the v3 source map through this.callback', () => {
		const { ctx, result } = fakeContext('/abs/X.ts')
		const ret = treatyLoader.call(ctx as never, TS_COMPONENT_SOURCE)

		// Delivered via this.callback (map present), so the loader returns nothing.
		expect(ret).toBeUndefined()

		const { error, content, map } = result()
		expect(error).toBeNull()
		expect(typeof content).toBe('string')
		expect(content).toContain('ɵɵdefineComponent')

		expect(map).toBeDefined()
		const parsed = map as { version?: number; mappings?: string }
		expect(parsed.version).toBe(3)
		expect(typeof parsed.mappings).toBe('string')
	})

	it('returns code directly when the front-end produced no map', () => {
		const { ctx, result } = fakeContext('/abs/Hello.treaty')
		const ret = treatyLoader.call(ctx as never, TREATY_SOURCE)

		// No map -> the code is returned directly (callback is not used for the map).
		expect(typeof ret).toBe('string')
		expect(ret as string).toContain('ɵɵdefineComponent')
		expect(result().map).toBeUndefined()
	})

	it('passes through modules it does not own', () => {
		const plain = 'export const answer = 42\n'
		const { ctx } = fakeContext('/abs/util.ts')
		const ret = treatyLoader.call(ctx as never, plain)
		expect(ret).toBe(plain)
	})
})
