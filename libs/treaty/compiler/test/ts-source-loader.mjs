/**
 * Minimal ESM resolver hook so the unit tests can import the package's TypeScript
 * SOURCE directly (under Node's built-in type-stripping) without a `tsc` build.
 *
 * The source uses NodeNext-style `.js` import specifiers that point at sibling
 * `.ts` files (rewritten to `.js` only on emit). At runtime no `.js` exists yet,
 * so this hook rewrites a relative `./x.js` specifier to `./x.ts` when the `.js`
 * is absent but the `.ts` is present. Everything else resolves normally.
 *
 * Register via:
 *   node --experimental-strip-types \
 *     --import ./test/register-ts-source.mjs test/<name>.mjs
 */

import { existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

export async function resolve(specifier, context, nextResolve) {
	if (
		(specifier.startsWith('./') || specifier.startsWith('../')) &&
		specifier.endsWith('.js') &&
		context.parentURL
	) {
		const resolved = new URL(specifier, context.parentURL)
		const jsPath = fileURLToPath(resolved)
		if (!existsSync(jsPath) && existsSync(jsPath.replace(/\.js$/, '.ts'))) {
			return nextResolve(specifier.replace(/\.js$/, '.ts'), context)
		}
	}
	return nextResolve(specifier, context)
}
