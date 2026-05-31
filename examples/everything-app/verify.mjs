// Compile-verify every showcase source through the real Treaty compiler.
//
// Imports the PUBLIC `@treaty/compiler` package (which routes to the prebuilt
// `@treaty/authoring-node` NAPI addon — no cargo / no rebuild) and compiles
// EACH example source to Ivy, asserting the expected output per file kind:
//
//   - component sources (.treaty, .tsx, @Component .ts) MUST emit the Ivy
//     `ɵɵdefineComponent` symbol;
//   - routes and standalone server modules ('use server' / 'use websocket')
//     are file-by-file pass-throughs at the compiler stage (the server-fn split
//     and federation wiring are the bundler/host's job), so they MUST compile
//     with zero diagnostics.
//
// Run: node examples/everything-app/verify.mjs   (from repo root)
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { compileTreaty, compileUnifiedSource } from '@treaty/compiler'

const here = dirname(fileURLToPath(import.meta.url))

const DEFINE_COMPONENT = 'ɵɵdefineComponent'

/** What we expect each source to lower to. */
const COMPONENT = 'component' // must emit ɵɵdefineComponent
const PASSTHROUGH = 'passthrough' // routes / server modules: must compile clean

/** @type {Array<{ rel: string, expect: string }>} */
const files = [
	{ rel: 'src/components/todo-list.treaty', expect: COMPONENT },
	{ rel: 'src/components/counter.tsx', expect: COMPONENT },
	{ rel: 'src/components/log-viewer.component.ts', expect: COMPONENT },
	{ rel: 'src/features/dashboard/dashboard.component.ts', expect: COMPONENT },
	{ rel: 'src/features/profile/profile.component.ts', expect: COMPONENT },
	{ rel: 'src/features/profile/profile-settings.component.ts', expect: COMPONENT },
	{ rel: 'src/features/greeter/greeter.treaty', expect: COMPONENT },
	{ rel: 'src/features/greeter/greeting-card.tjsx', expect: COMPONENT },
	{ rel: 'src/features/greeter/greeter-page.component.ts', expect: COMPONENT },
	{ rel: 'src/features/metrics/gauge.treaty', expect: COMPONENT },
	{ rel: 'src/features/metrics/metrics-panel.component.ts', expect: COMPONENT },
	{ rel: 'src/features/metrics/highlight-delta.directive.ts', expect: PASSTHROUGH },
	{ rel: 'src/features/metrics/percent.pipe.ts', expect: PASSTHROUGH },
	{ rel: 'src/features/greeter/greeting.types.ts', expect: PASSTHROUGH },
	{ rel: 'src/features/profile/profile.routes.ts', expect: PASSTHROUGH },
	{ rel: 'src/routes/app.routes.ts', expect: PASSTHROUGH },
	{ rel: 'src/server/todos.server.ts', expect: PASSTHROUGH },
	{ rel: 'src/server/presence.ws.ts', expect: PASSTHROUGH },
	{ rel: 'src/server/logs.stream.ts', expect: PASSTHROUGH },
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

	// COMPONENT sources must lower to an Ivy ɵɵdefineComponent definition;
	// PASSTHROUGH sources (routes / standalone server modules) only need a
	// clean, diagnostic-free compile.
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
