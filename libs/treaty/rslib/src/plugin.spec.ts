/**
 * Smoke specs for the Treaty rslib/rsbuild plugin's source-map forwarding. They
 * drive {@link treatyRsbuildPlugin} against a hand-rolled, structurally-typed
 * `TreatyRsbuildPluginApi` (no real rslib) and assert that the registered
 * transform handler hands back the v3 source map the core produced — in the
 * `{ code, map }` shape rslib/rsbuild consume — when one is present, and omits it
 * for files it does not own. They run against the real `@treaty/compiler` core
 * (which routes through the Rust authoring compiler).
 */

import { describe, expect, it } from 'bun:test'
import { treatyRsbuildPlugin, TREATY_PLUGIN_NAME, TREATY_TRANSFORM_TEST } from './plugin.ts'
import type {
	TreatyRslibPluginOptions,
	TreatyTransformContext,
	TreatyTransformHandler,
	TreatyTransformOutput,
} from './types.ts'

/** A `.ts` `@Component` source: the base-Angular front-end emits a v3 map for it. */
const TS_COMPONENT_SOURCE =
	"import { Component } from '@angular/core'\n" +
	"@Component({ selector: 'app-x', template: '<div>x</div>' })\n" +
	'export class XComponent {}\n'

/**
 * Minimal `TreatyRsbuildPluginApi` capturing the registered transform handler.
 * `processAssets`/`onBeforeBuild` are omitted (optional) so the plugin only wires
 * the transform, which is what these specs exercise.
 */
function fakeApi(): {
	api: {
		transform: (descriptor: { test: RegExp }, handler: TreatyTransformHandler) => void
	}
	handler: () => TreatyTransformHandler | undefined
	test: () => RegExp | undefined
} {
	let handler: TreatyTransformHandler | undefined
	let test: RegExp | undefined
	const api = {
		transform(descriptor: { test: RegExp }, h: TreatyTransformHandler): void {
			test = descriptor.test
			handler = h
		},
	}
	return { api, handler: () => handler, test: () => test }
}

function setupPlugin(options: TreatyRslibPluginOptions = {}): TreatyTransformHandler {
	const { api, handler } = fakeApi()
	treatyRsbuildPlugin(options).setup(api as never)
	const h = handler()
	if (!h) throw new Error('plugin did not register a transform handler')
	return h
}

function asOutput(value: TreatyTransformOutput | string | null): TreatyTransformOutput {
	if (value === null || typeof value === 'string') {
		throw new Error('expected a { code, map } transform output')
	}
	return value
}

describe('treatyRsbuildPlugin source maps', () => {
	it('has the stable plugin name and transform test', () => {
		expect(treatyRsbuildPlugin().name).toBe(TREATY_PLUGIN_NAME)
		expect(TREATY_TRANSFORM_TEST.test('App.tsx')).toBe(true)
		expect(TREATY_TRANSFORM_TEST.test('greeting.treaty')).toBe(true)
		expect(TREATY_TRANSFORM_TEST.test('styles.css')).toBe(false)
	})

	it('forwards the v3 source map in { code, map } shape', () => {
		const handler = setupPlugin()
		const ctx: TreatyTransformContext = { code: TS_COMPONENT_SOURCE, resource: '/abs/X.ts' }
		const out = asOutput(handler(ctx))

		expect(out.code).toContain('ɵɵdefineComponent')
		expect(typeof out.map).toBe('string')
		const parsed = JSON.parse(out.map as string) as { version?: number; mappings?: string }
		expect(parsed.version).toBe(3)
		expect(typeof parsed.mappings).toBe('string')
	})

	it('passes through unowned modules as the original code (no map)', () => {
		const handler = setupPlugin()
		// A plain `.ts` without `@Component` is not Treaty's: the handler returns the
		// original source string (the core returned null), carrying no map.
		const plain = 'export const answer = 42\n'
		const out = handler({ code: plain, resource: '/abs/util.ts' })
		expect(out).toBe(plain)
	})
})
