/**
 * Smoke specs for the Treaty Rsbuild plugin's source-map forwarding. They drive
 * {@link pluginTreaty} against a hand-rolled, structurally-typed `RsbuildPluginAPI`
 * (no real Rsbuild) and assert that the `api.transform` handler hands back the v3
 * source map the core produced — in Rsbuild's native `{ code, map }` shape — when
 * one is present, and omits it otherwise. They run against the real
 * `@treaty/compiler` core (which routes through the Rust authoring compiler).
 */

import { describe, expect, it } from 'bun:test'
import { pluginTreaty, PLUGIN_NAME } from './plugin.ts'
import type { TreatyPluginOptions } from './options.ts'

/** A `.ts` `@Component` source: the base-Angular front-end emits a v3 map for it. */
const TS_COMPONENT_SOURCE =
	"import { Component } from '@angular/core'\n" +
	"@Component({ selector: 'app-x', template: '<div>x</div>' })\n" +
	'export class XComponent {}\n'

/** The shape a Treaty transform handler returns. */
interface TransformOut {
	readonly code: string
	readonly map?: string
}

/** The transform handler the plugin registers via `api.transform`. */
type TransformHandler = (args: {
	code: string
	resourcePath: string
}) => TransformOut | Promise<TransformOut>

/**
 * A minimal `RsbuildPluginAPI` that captures the registered transform handler and
 * the `modifyRsbuildConfig` callbacks. `transform` is a function so the plugin
 * takes its first-class transform path (Strategy 1) rather than the loader fallback.
 */
function fakeApi(): {
	api: { transform: (descriptor: { test: RegExp }, handler: TransformHandler) => void } & Record<
		string,
		unknown
	>
	handler: () => TransformHandler | undefined
	test: () => RegExp | undefined
} {
	let handler: TransformHandler | undefined
	let test: RegExp | undefined
	const api = {
		transform(descriptor: { test: RegExp }, h: TransformHandler): void {
			test = descriptor.test
			handler = h
		},
		modifyRsbuildConfig(): void {
			// resolve.extensions wiring is exercised elsewhere; ignored here.
		},
	}
	return { api, handler: () => handler, test: () => test }
}

function setupPlugin(options: TreatyPluginOptions = {}): {
	handler: TransformHandler
	test: RegExp
} {
	const { api, handler, test } = fakeApi()
	pluginTreaty(options).setup(api as never)
	const h = handler()
	const t = test()
	if (!h || !t) throw new Error('plugin did not register a transform handler')
	return { handler: h, test: t }
}

describe('pluginTreaty source maps', () => {
	it('has the stable plugin name', () => {
		expect(pluginTreaty().name).toBe(PLUGIN_NAME)
	})

	it('forwards the v3 source map in Rsbuild { code, map } shape', async () => {
		const { handler } = setupPlugin()
		const out = await handler({ code: TS_COMPONENT_SOURCE, resourcePath: '/abs/X.ts' })

		expect(out.code).toContain('ɵɵdefineComponent')
		// The core produced a serialized v3 map; the plugin must forward it unchanged.
		expect(typeof out.map).toBe('string')
		const parsed = JSON.parse(out.map as string) as { version?: number; mappings?: string }
		expect(parsed.version).toBe(3)
		expect(typeof parsed.mappings).toBe('string')
		// CLIENT PRIVACY: this fixture has no server block, so nothing to redact —
		// but the map is the addon's, never synthesized here.
	})

	it('passes through unowned modules with no map', async () => {
		const { handler } = setupPlugin()
		// A plain `.ts` without `@Component` is not Treaty's: the handler returns the
		// original code and no map (the core returned null).
		const plain = 'export const answer = 42\n'
		const out = await handler({ code: plain, resourcePath: '/abs/util.ts' })
		expect(out.code).toBe(plain)
		expect(out.map).toBeUndefined()
	})
})
