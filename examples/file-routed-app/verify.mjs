// Compile-verify every file-routed source through the real Treaty compiler.
//
// Imports the PUBLIC `@treaty/compiler` package (which routes to the prebuilt
// `@treaty/authoring-node` NAPI addon — no cargo / no rebuild) and compiles each
// source to Ivy, asserting the expected output per file kind:
//
//   - route components (.treaty / .tjsx) MUST emit the Ivy `ɵɵdefineComponent`
//     symbol;
//   - the eager bootstrap root (@Component .ts) likewise;
//   - api handlers ('use server' modules) are file-by-file pass-throughs at the
//     compiler stage (the server-fn split + endpoint manifest are the
//     bundler/host's job), so they MUST compile with zero diagnostics.
//
// Run: node examples/file-routed-app/verify.mjs   (from repo root)
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { compileTreaty, compileUnifiedSource } from '@treaty/compiler'

const here = dirname(fileURLToPath(import.meta.url))

const DEFINE_COMPONENT = 'ɵɵdefineComponent'

const COMPONENT = 'component' // must emit ɵɵdefineComponent
const PASSTHROUGH = 'passthrough' // api handlers: must compile clean

/** @type {Array<{ rel: string, expect: string }>} */
const files = [
	// Eager bootstrap root.
	{ rel: 'src/app/routed-root.component.ts', expect: COMPONENT },
	// Route components — every routable file in the routes/ tree.
	{ rel: 'routes/layout.treaty', expect: COMPONENT },
	{ rel: 'routes/index.treaty', expect: COMPONENT },
	{ rel: 'routes/not-found.treaty', expect: COMPONENT },
	{ rel: 'routes/(marketing)/index.treaty', expect: COMPONENT },
	{ rel: 'routes/(marketing)/about.tjsx', expect: COMPONENT },
	{ rel: 'routes/blog/layout.treaty', expect: COMPONENT },
	{ rel: 'routes/blog/index.treaty', expect: COMPONENT },
	{ rel: 'routes/blog/[slug]/index.treaty', expect: COMPONENT },
	{ rel: 'routes/blog/[...path]/index.treaty', expect: COMPONENT },
	{ rel: 'routes/docs/[category]/[page]/index.tjsx', expect: COMPONENT },
	// Api handlers — server modules, pass-through at the compiler stage.
	{ rel: 'api/index.ts', expect: PASSTHROUGH },
	{ rel: 'api/health/index.ts', expect: PASSTHROUGH },
	{ rel: 'api/posts/index.ts', expect: PASSTHROUGH },
	{ rel: 'api/posts/[id]/index.ts', expect: PASSTHROUGH },
]

let failed = 0
const results = []

for (const { rel, expect } of files) {
	const code = readFileSync(join(here, rel), 'utf8')
	const out = rel.endsWith('.treaty') ? compileTreaty(code, rel) : compileUnifiedSource(code, rel)
	const errors = out.errors ?? []

	if (errors.length) {
		failed++
		results.push(`FAIL ${rel}\n  diagnostics:\n    ${errors.join('\n    ')}`)
		continue
	}

	const hasDefine = out.code.includes(DEFINE_COMPONENT)
	const problem =
		expect === COMPONENT && !hasDefine
			? `expected ${DEFINE_COMPONENT} in emitted Ivy, none found`
			: null

	if (problem) {
		failed++
		results.push(`FAIL ${rel}  [${expect}]\n  ${problem}`)
	} else {
		const note = expect === COMPONENT ? `${DEFINE_COMPONENT} ✓` : 'clean ✓'
		results.push(`OK   ${rel}  [${expect}]  (code ${out.code.length}b, ${note})`)
	}
}

console.log(results.join('\n'))
console.log(
	failed ? `\n${failed} of ${files.length} file(s) failed` : `\nAll ${files.length} files verified`
)
process.exit(failed ? 1 : 0)
