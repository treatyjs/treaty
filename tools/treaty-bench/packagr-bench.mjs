// @ts-check
/**
 * treaty-packagr  vs  Angular ng-packagr  —  same library, head-to-head.
 *
 * Builds ONE standard-Angular component library (tools/treaty-bench/sample-lib:
 * two plain `@Component` `.ts` classes + a `public-api.ts` barrel) with BOTH
 * packagers, measures wall-clock build time + emitted dist bytes, then proves the
 * two emit the SAME Ivy by normalizing and diffing the lowered `ɵɵdefineComponent`
 * per component, plus the `.d.ts` `ɵcmp` declaration and the published exports map.
 *
 * Why a plain-`@Component` library: ng-packagr only understands standard Angular
 * TypeScript (it runs ngtsc), so the sample MUST be authored that way for an
 * apples-to-apples comparison. treaty-packagr lowers the same `@Component` source
 * through its `treaty_ivy` front-end. The example libs under examples/ are
 * authored in `.treaty`/`.tsx` (Treaty-only surfaces ng-packagr cannot read), so a
 * small standard-Angular lib is the only fair shared input.
 *
 * Apples-to-apples Ivy: ng-packagr defaults to PARTIAL compilation
 * (`ɵɵngDeclareComponent`); treaty-packagr emits FULL AOT (`ɵɵdefineComponent`).
 * To compare the same artifact we drive ng-packagr in `compilationMode: 'full'`
 * so BOTH emit `ɵɵdefineComponent`, and diff those blocks.
 *
 * The two packagers' dist SHAPES differ by design (ng-packagr inlines every
 * component into one APF `fesm2022/<name>.mjs`; treaty-packagr emits per-entry
 * `index.mjs`). The benchmark therefore packages each component as its OWN treaty
 * secondary entry so treaty emits a directly-comparable per-component
 * `ɵɵdefineComponent`, and the equivalence verdict is over the lowered Ivy +
 * `.d.ts` `ɵcmp` shape, NOT the wrapping bundle layout.
 *
 * treaty-packagr has no NAPI binding, so it is invoked via a tiny release Rust
 * runner (libs/packagr/examples/packagr_runner.rs) that calls
 * `treaty_packagr::build_to_disk` and prints in-process timing as JSON. This file
 * builds that runner once (cargo, release) if it is missing.
 *
 * This is a standalone .mjs benchmark: it MAY freely use performance.now() /
 * Date.now() for timing (the workflow-script clock restriction does not apply).
 *
 * Usage:  node tools/treaty-bench/packagr-bench.mjs [--runs N]
 * Writes: tools/treaty-bench/results/packagr.json
 */

import { createRequire } from 'node:module'
import { execFileSync } from 'node:child_process'
import { performance } from 'node:perf_hooks'
import {
	readFileSync,
	writeFileSync,
	readdirSync,
	existsSync,
	mkdirSync,
	rmSync,
	statSync,
} from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import os from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const req = createRequire(join(repoRoot, 'noop.cjs'))

const RUNS = (() => {
	const i = process.argv.indexOf('--runs')
	if (i >= 0 && process.argv[i + 1]) return Math.max(1, Number(process.argv[i + 1]) | 0)
	return 3
})()

const sampleLib = join(here, 'sample-lib')
const resultsDir = join(here, 'results')

// ---------------------------------------------------------------------------
// The shared sample library. Two plain-`@Component` `.ts` classes (one with a
// classic `@Input`, one with an event listener) + a barrel. Authored in standard
// Angular TS so BOTH packagers can compile it. Each component is also declared as
// a treaty SECONDARY entry, so treaty-packagr emits a per-component
// `ɵɵdefineComponent` (its barrel-only primary does not inline components into the
// dist — see notes), giving a directly-comparable Ivy artifact.
// ---------------------------------------------------------------------------
const COMPONENTS = [
	{
		name: 'HelloComponent',
		selector: 'tb-hello',
		file: 'hello.component.ts',
		sub: 'hello',
		source:
			"import { Component, Input } from '@angular/core';\n\n" +
			"@Component({\n" +
			"  selector: 'tb-hello',\n" +
			"  standalone: true,\n" +
			"  template: '<h1 class=\"greeting\">Hello {{ name }}</h1>',\n" +
			"})\n" +
			"export class HelloComponent {\n" +
			"  @Input() name: string = 'World';\n" +
			"}\n",
	},
	{
		name: 'CounterComponent',
		selector: 'tb-counter',
		file: 'counter.component.ts',
		sub: 'counter',
		source:
			"import { Component } from '@angular/core';\n\n" +
			"@Component({\n" +
			"  selector: 'tb-counter',\n" +
			"  standalone: true,\n" +
			"  template: '<button (click)=\"inc()\">count {{ count }}</button>',\n" +
			"})\n" +
			"export class CounterComponent {\n" +
			"  count: number = 0;\n" +
			"  inc(): void {\n" +
			"    this.count++;\n" +
			"  }\n" +
			"}\n",
	},
]

function writeSampleLib() {
	rmSync(sampleLib, { recursive: true, force: true })
	mkdirSync(join(sampleLib, 'src'), { recursive: true })

	for (const c of COMPONENTS) {
		writeFileSync(join(sampleLib, 'src', c.file), c.source)
	}
	writeFileSync(
		join(sampleLib, 'src', 'public-api.ts'),
		COMPONENTS.map((c) => `export * from './${c.file.replace(/\.ts$/, '')}';`).join('\n') + '\n',
	)

	// A sibling package.json (name/version fallback, mirrors a real lib).
	writeFileSync(
		join(sampleLib, 'package.json'),
		JSON.stringify({ name: '@treaty-bench/widgets', version: '1.0.0' }, null, 2) + '\n',
	)

	// treaty descriptor: primary barrel + each component as a secondary entry so
	// treaty-packagr emits a per-component `ɵɵdefineComponent` index.mjs to compare.
	writeFileSync(
		join(sampleLib, 'treaty-package.json'),
		JSON.stringify(
			{
				name: '@treaty-bench/widgets',
				version: '1.0.0',
				dest: 'dist',
				lib: { entryFile: 'src/public-api.ts' },
				secondaryEntryPoints: COMPONENTS.map((c) => ({
					path: c.sub,
					entryFile: `src/${c.file}`,
				})),
			},
			null,
			2,
		) + '\n',
	)

	// ng-packagr full-compilation project + tsconfig.
	writeFileSync(
		join(sampleLib, 'ng-package.full.json'),
		JSON.stringify({ dest: 'dist-ng-full', lib: { entryFile: 'src/public-api.ts' } }, null, 2) + '\n',
	)
	writeFileSync(
		join(sampleLib, 'tsconfig.full.json'),
		JSON.stringify(
			{
				compilerOptions: {
					target: 'ES2022',
					module: 'ES2022',
					moduleResolution: 'bundler',
					declaration: true,
					strict: false,
					skipLibCheck: true,
					experimentalDecorators: true,
					emitDecoratorMetadata: false,
					lib: ['ES2022', 'dom'],
					types: [],
				},
				// FULL mode so ng-packagr emits `ɵɵdefineComponent` (AOT) — the same
				// artifact treaty-packagr emits — instead of its default partial
				// `ɵɵngDeclareComponent`.
				angularCompilerOptions: { strictTemplates: false, compilationMode: 'full' },
				files: ['src/public-api.ts'],
			},
			null,
			2,
		) + '\n',
	)
}

// ---------------------------------------------------------------------------
// Sizing.
// ---------------------------------------------------------------------------
function dirBytes(dir) {
	if (!existsSync(dir)) return 0
	let total = 0
	for (const entry of readdirSync(dir, { recursive: true })) {
		if (typeof entry !== 'string') continue
		const p = join(dir, entry)
		try {
			const st = statSync(p)
			if (st.isFile()) total += st.size
		} catch {
			/* ignore */
		}
	}
	return total
}

// ---------------------------------------------------------------------------
// treaty-packagr: build the release Rust runner once, then invoke it per run.
// ---------------------------------------------------------------------------
const RUNNER = join(repoRoot, 'target', 'release', 'examples', 'packagr_runner.exe')
const RUNNER_NIX = join(repoRoot, 'target', 'release', 'examples', 'packagr_runner')

function runnerPath() {
	if (existsSync(RUNNER)) return RUNNER
	if (existsSync(RUNNER_NIX)) return RUNNER_NIX
	return null
}

function buildRunner() {
	if (runnerPath()) return runnerPath()
	console.log('-- building packagr_runner (cargo --release) — first run only, may take minutes --')
	execFileSync(
		'cargo',
		['build', '-p', 'treaty_packagr', '--release', '--example', 'packagr_runner'],
		{ cwd: repoRoot, stdio: ['ignore', 'inherit', 'inherit'] },
	)
	return runnerPath()
}

// One treaty-packagr build. Returns { ms, dest, entries } parsed from the runner's
// JSON stdout. The runner reports its OWN in-process `build_to_disk` time (ms),
// which excludes process startup — the fair measure of the packaging work.
function buildTreaty(runner) {
	rmSync(join(sampleLib, 'dist'), { recursive: true, force: true })
	const wallStart = performance.now()
	const out = execFileSync(runner, [sampleLib], { encoding: 'utf-8' })
	const wallMs = performance.now() - wallStart
	const line = out.trim().split('\n').filter(Boolean).pop()
	const parsed = JSON.parse(line)
	if (!parsed.ok) throw new Error(`treaty-packagr failed: ${parsed.error}`)
	return { ms: parsed.ms, wallMs, dest: join(sampleLib, 'dist') }
}

// ---------------------------------------------------------------------------
// ng-packagr (full mode), programmatic API. Returns { ms, dest }.
// ---------------------------------------------------------------------------
function ngPackagrAvailable() {
	try {
		req.resolve('ng-packagr', { paths: [repoRoot] })
		return true
	} catch {
		return false
	}
}

async function buildNgPackagr() {
	const dest = join(sampleLib, 'dist-ng-full')
	rmSync(dest, { recursive: true, force: true })
	const ngpMod = req(req.resolve('ng-packagr', { paths: [repoRoot] }))
	const ngPackagr = ngpMod.ngPackagr || (ngpMod.default && ngpMod.default.ngPackagr)
	if (!ngPackagr) throw new Error('ng-packagr export `ngPackagr` not found')
	const t0 = performance.now()
	await ngPackagr()
		.forProject(join(sampleLib, 'ng-package.full.json'))
		.withTsConfig(join(sampleLib, 'tsconfig.full.json'))
		.build()
	return { ms: performance.now() - t0, dest }
}

// ---------------------------------------------------------------------------
// best-of-N timing wrapper. `runFn` returns { ms, ... }. Captures the LAST run's
// extra fields (dest) and the MIN ms across runs.
// ---------------------------------------------------------------------------
async function measure(label, runFn) {
	const times = []
	let last = null
	let firstError = null
	for (let i = 0; i < RUNS; i++) {
		try {
			const r = await runFn()
			times.push(r.ms)
			last = r
		} catch (err) {
			firstError = err
			break
		}
	}
	if (firstError || times.length === 0) {
		return {
			tool: label,
			status: 'failed',
			note: String(firstError?.message ?? firstError ?? 'no successful run')
				.split('\n')
				.slice(0, 4)
				.join(' '),
		}
	}
	return {
		tool: label,
		status: 'measured',
		buildMs: Math.round(Math.min(...times) * 100) / 100,
		distBytes: dirBytes(last.dest),
		dest: last.dest,
		note: `best of ${times.length} run(s); times(ms)=[${times.map((t) => Math.round(t)).join(', ')}]`,
	}
}

// ===========================================================================
// EQUIVALENCE: normalize + diff the lowered Ivy.
// ===========================================================================

// Pull the balanced `ɵɵdefineComponent({ ... })` argument object out of `code`,
// returning the `{...}` text (brace-balanced, quote/comment-aware so a `}` inside
// a string or `/*@__PURE__*/` does not end it early). Returns null if absent.
function extractDefineComponent(code) {
	const marker = 'ɵɵdefineComponent('
	const at = code.indexOf(marker)
	if (at < 0) return null
	// Find the opening brace of the argument object.
	let i = code.indexOf('{', at)
	if (i < 0) return null
	const start = i
	let depth = 0
	let inStr = null
	let inLineComment = false
	let inBlockComment = false
	for (; i < code.length; i++) {
		const ch = code[i]
		const next = code[i + 1]
		if (inLineComment) {
			if (ch === '\n') inLineComment = false
			continue
		}
		if (inBlockComment) {
			if (ch === '*' && next === '/') {
				inBlockComment = false
				i++
			}
			continue
		}
		if (inStr) {
			if (ch === '\\') {
				i++
				continue
			}
			if (ch === inStr) inStr = null
			continue
		}
		if (ch === '/' && next === '/') {
			inLineComment = true
			i++
			continue
		}
		if (ch === '/' && next === '*') {
			inBlockComment = true
			i++
			continue
		}
		if (ch === '"' || ch === "'" || ch === '`') {
			inStr = ch
			continue
		}
		if (ch === '{') depth++
		else if (ch === '}') {
			depth--
			if (depth === 0) return code.slice(start, i + 1)
		}
	}
	return null
}

// Normalize an emitted-Ivy fragment so the two packagers' output is comparable.
//   - unify the `i0.`/`i1.` import-alias prefix → bare (parity.mjs does this)
//   - drop `/*@__PURE__*/` annotations (ng-packagr adds them; treaty omits)
//   - normalize string-quote style ' → "
//   - drop the `__ngFactoryType__` parameter-name nuance is irrelevant (same name)
//   - collapse all whitespace
// The instruction NAMES, argument ORDER and literal VALUES are preserved (these
// are load-bearing in Ivy — never sorted/reordered), exactly like parity.mjs.
function normalizeIvy(code) {
	let s = code
	s = s.replace(/\/\*\s*@__PURE__\s*\*\//g, '') // strip pure annotations
	s = s.replace(/\bi\d+\./g, '') // i0.ɵɵx -> ɵɵx
	s = s.replace(/'([^'\\]*)'/g, '"$1"') // ' -> "
	s = s.replace(/\s+/g, '') // collapse whitespace
	return s
}

// Normalize a `.d.ts` `ɵcmp: i0.ɵɵComponentDeclaration<...>` declaration for diff:
// unify the import alias and collapse whitespace. Returns the full
// `ɵɵComponentDeclaration<...>` generic, brace/angle-balanced, or null.
function extractCmpDeclaration(dts) {
	const marker = 'ɵɵComponentDeclaration<'
	const at = dts.indexOf(marker)
	if (at < 0) return null
	let i = at + marker.length
	let depth = 1
	let inStr = null
	const start = at
	for (; i < dts.length; i++) {
		const ch = dts[i]
		if (inStr) {
			if (ch === '\\') {
				i++
				continue
			}
			if (ch === inStr) inStr = null
			continue
		}
		if (ch === '"' || ch === "'") {
			inStr = ch
			continue
		}
		if (ch === '<') depth++
		else if (ch === '>') {
			depth--
			if (depth === 0) return dts.slice(start, i + 1)
		}
	}
	return null
}

function normalizeDts(code) {
	return code.replace(/\bi\d+\./g, '').replace(/import\("[^"]*"\)\./g, '').replace(/\s+/g, '')
}

function firstDiff(a, b) {
	const n = Math.min(a.length, b.length)
	for (let i = 0; i < n; i++) {
		if (a[i] !== b[i]) {
			const start = Math.max(0, i - 25)
			return { index: i, treaty: a.slice(start, i + 35), ngp: b.slice(start, i + 35) }
		}
	}
	if (a.length !== b.length) {
		return { index: n, treaty: a.slice(Math.max(0, n - 35)), ngp: b.slice(Math.max(0, n - 35)) }
	}
	return null
}

// Compare the two dist outputs for Ivy equivalence, per component.
function compareOutputs(treatyDest, ngpDest) {
	const ngpMjs = readFileSync(join(ngpDest, 'fesm2022', 'treaty-bench-widgets.mjs'), 'utf-8')
	const ngpDts = readFileSync(join(ngpDest, 'types', 'treaty-bench-widgets.d.ts'), 'utf-8')

	const perComponent = []
	for (const c of COMPONENTS) {
		// treaty emits per-component index.mjs / index.d.ts under dist/<sub>/.
		const tMjs = readFileSync(join(treatyDest, c.sub, 'index.mjs'), 'utf-8')
		const tDts = readFileSync(join(treatyDest, c.sub, 'index.d.ts'), 'utf-8')

		// --- emitted Ivy define block ---
		const tDef = extractDefineComponent(tMjs)
		// ng-packagr inlines both components into one fesm; slice the per-class region
		// so we extract the right component's define block.
		const ngpClassAt = ngpMjs.indexOf(`class ${c.name}`)
		const ngpRegion = ngpClassAt >= 0 ? ngpMjs.slice(ngpClassAt) : ngpMjs
		const ngpDef = extractDefineComponent(ngpRegion)

		const ivyTreaty = tDef ? normalizeIvy(tDef) : null
		const ivyNgp = ngpDef ? normalizeIvy(ngpDef) : null
		const ivyDiff = ivyTreaty && ivyNgp ? firstDiff(ivyTreaty, ivyNgp) : { index: -1, reason: 'missing define block' }
		const ivyEqual = ivyTreaty != null && ivyNgp != null && ivyDiff === null

		// --- .d.ts ɵcmp declaration ---
		const tCmpRegion = tDts.slice(Math.max(0, tDts.indexOf(c.name)))
		const ngpCmpRegion = ngpDts.slice(Math.max(0, ngpDts.indexOf(`class ${c.name}`)))
		const tCmp = extractCmpDeclaration(tCmpRegion)
		const ngpCmp = extractCmpDeclaration(ngpCmpRegion)
		const dtsTreaty = tCmp ? normalizeDts(tCmp) : null
		const dtsNgp = ngpCmp ? normalizeDts(ngpCmp) : null
		const dtsDiff = dtsTreaty && dtsNgp ? firstDiff(dtsTreaty, dtsNgp) : { index: -1, reason: 'missing ɵcmp decl' }
		const dtsEqual = dtsTreaty != null && dtsNgp != null && dtsDiff === null

		perComponent.push({
			component: c.name,
			ivyEqual,
			ivyDiff: ivyEqual ? null : ivyDiff,
			ivyTreatyLen: ivyTreaty ? ivyTreaty.length : 0,
			ivyNgpLen: ivyNgp ? ivyNgp.length : 0,
			dtsEqual,
			dtsDiff: dtsEqual ? null : dtsDiff,
			dtsTreaty: dtsTreaty,
			dtsNgp: dtsNgp,
		})
	}

	// --- package.json exports parity (both must export every component under "." ) ---
	const tPkg = JSON.parse(readFileSync(join(treatyDest, 'package.json'), 'utf-8'))
	const ngpPkg = JSON.parse(readFileSync(join(ngpDest, 'package.json'), 'utf-8'))
	const pkg = {
		name: { treaty: tPkg.name, ngp: ngpPkg.name, equal: tPkg.name === ngpPkg.name },
		version: { treaty: tPkg.version, ngp: ngpPkg.version, equal: tPkg.version === ngpPkg.version },
		type: { treaty: tPkg.type, ngp: ngpPkg.type, equal: tPkg.type === ngpPkg.type },
		sideEffects: {
			treaty: tPkg.sideEffects,
			ngp: ngpPkg.sideEffects,
			equal: tPkg.sideEffects === ngpPkg.sideEffects,
		},
		// Both have a "." export; the target FILE differs (treaty: ./index.mjs,
		// ngp: ./fesm2022/...). We compare that a "." export EXISTS in both.
		hasPrimaryExport: {
			treaty: Boolean(tPkg.exports && tPkg.exports['.']),
			ngp: Boolean(ngpPkg.exports && ngpPkg.exports['.']),
			equal:
				Boolean(tPkg.exports && tPkg.exports['.']) === Boolean(ngpPkg.exports && ngpPkg.exports['.']),
		},
	}

	const ivyAllEqual = perComponent.every((p) => p.ivyEqual)
	const dtsAllEqual = perComponent.every((p) => p.dtsEqual)
	return { perComponent, pkg, ivyAllEqual, dtsAllEqual }
}

// ---------------------------------------------------------------------------
function pkgVersion(name) {
	try {
		const p = req.resolve(`${name}/package.json`, { paths: [repoRoot] })
		return JSON.parse(readFileSync(p, 'utf-8')).version
	} catch {
		return null
	}
}

async function main() {
	console.log(`== treaty-packagr vs ng-packagr (lib: tools/treaty-bench/sample-lib, runs=${RUNS}) ==`)
	mkdirSync(resultsDir, { recursive: true })
	writeSampleLib()

	const results = []
	let comparison = null
	let comparisonError = null

	// ---- treaty-packagr ----
	console.log('-- treaty-packagr (Rust, full AOT) --')
	let treatyRes
	try {
		const runner = buildRunner()
		if (!runner) throw new Error('packagr_runner not built and cargo build produced no binary')
		treatyRes = await measure('treaty-packagr', () => Promise.resolve(buildTreaty(runner)))
	} catch (err) {
		treatyRes = {
			tool: 'treaty-packagr',
			status: 'failed',
			note: String(err?.message ?? err).split('\n').slice(0, 4).join(' '),
		}
	}
	results.push(treatyRes)
	console.log(`   ${treatyRes.status}  ${treatyRes.buildMs ?? '—'} ms  ${treatyRes.distBytes ?? '—'} B`)

	// ---- ng-packagr ----
	console.log('-- ng-packagr (full compilation mode) --')
	let ngpRes
	if (!ngPackagrAvailable()) {
		ngpRes = {
			tool: 'ng-packagr',
			status: 'pending',
			note: 'ng-packagr not resolvable from repo root node_modules',
		}
	} else {
		ngpRes = await measure('ng-packagr', () => buildNgPackagr())
	}
	results.push(ngpRes)
	console.log(`   ${ngpRes.status}  ${ngpRes.buildMs ?? '—'} ms  ${ngpRes.distBytes ?? '—'} B`)

	// ---- equivalence ----
	if (treatyRes.status === 'measured' && ngpRes.status === 'measured') {
		console.log('-- comparing emitted Ivy / .d.ts / exports --')
		try {
			comparison = compareOutputs(treatyRes.dest, ngpRes.dest)
		} catch (err) {
			comparisonError = String(err?.message ?? err)
		}
	}

	// ---- speed verdict ----
	let speedNote = null
	if (treatyRes.status === 'measured' && ngpRes.status === 'measured') {
		const ratio = ngpRes.buildMs / treatyRes.buildMs
		speedNote = `treaty-packagr ${treatyRes.buildMs} ms vs ng-packagr ${ngpRes.buildMs} ms — treaty-packagr is ${ratio.toFixed(1)}x faster`
	}

	// ---- emit JSON ----
	const out = {
		benchmark: 'packagr-treaty-vs-ngpackagr',
		generatedAt: new Date().toISOString(),
		library: {
			path: 'tools/treaty-bench/sample-lib',
			note: 'Standard-Angular library: 2 plain @Component .ts classes (one @Input, one event listener) + a public-api.ts barrel — authored as standard Angular TS so BOTH packagers can compile it.',
			components: COMPONENTS.map((c) => ({ name: c.name, selector: c.selector, file: c.file })),
		},
		host: {
			platform: process.platform,
			arch: process.arch,
			node: process.version,
			cpu: os.cpus()[0]?.model?.trim() || 'unknown',
		},
		runsPerTool: RUNS,
		versions: {
			'ng-packagr': pkgVersion('ng-packagr'),
			'@angular/core': pkgVersion('@angular/core'),
			'@angular/compiler-cli': pkgVersion('@angular/compiler-cli'),
			typescript: pkgVersion('typescript'),
		},
		metric:
			'treaty-packagr buildMs = its own in-process build_to_disk() time (compile every entry to Ivy + .d.ts + write APF dist); ng-packagr buildMs = ngPackagr().build() wall time. best (min) of N. distBytes = sum of emitted dist files.',
		ivyComparison:
			'ng-packagr driven in compilationMode:"full" so BOTH emit ɵɵdefineComponent; per-component define block + .d.ts ɵcmp decl normalized (unify i0 alias, drop /*@__PURE__*/, "->", collapse whitespace; arg ORDER/VALUES preserved) and diffed.',
		results,
		equivalence: comparison
			? {
					ivyAllEqual: comparison.ivyAllEqual,
					dtsAllEqual: comparison.dtsAllEqual,
					perComponent: comparison.perComponent,
					packageJson: comparison.pkg,
				}
			: { status: 'not-run', reason: comparisonError || 'one or both packagers did not produce a measured build' },
		speedNote,
	}
	const outPath = join(resultsDir, 'packagr.json')
	writeFileSync(outPath, JSON.stringify(out, null, 2) + '\n')

	// ---- console summary ----
	console.log('')
	console.log('== results ==')
	for (const r of results) {
		const size = r.distBytes != null ? `${(r.distBytes / 1024).toFixed(1)} KiB` : '—'
		const ms = r.buildMs != null ? `${r.buildMs} ms` : '—'
		console.log(`  ${r.tool.padEnd(16)} ${r.status.padEnd(9)} ${ms.padStart(10)}  ${size.padStart(11)}`)
		if (r.note) console.log(`      ${r.note}`)
	}
	if (speedNote) console.log(`\n  ${speedNote}`)
	if (comparison) {
		console.log('\n== Ivy / .d.ts equivalence ==')
		for (const p of comparison.perComponent) {
			console.log(
				`  ${p.component.padEnd(18)} ivy=${p.ivyEqual ? 'EQUAL' : 'DIFF'}  dts=${p.dtsEqual ? 'EQUAL' : 'DIFF'}`,
			)
			if (!p.ivyEqual && p.ivyDiff)
				console.log(`      ivy first diff @${p.ivyDiff.index}: treaty=...${p.ivyDiff.treaty} | ngp=...${p.ivyDiff.ngp}`)
			if (!p.dtsEqual && p.dtsDiff) {
				console.log(`      dts treaty: ${p.dtsTreaty}`)
				console.log(`      dts ngp:    ${p.dtsNgp}`)
			}
		}
		console.log(
			`\n  VERDICT: emitted Ivy ${comparison.ivyAllEqual ? 'EQUAL across all components' : 'DIFFERS'}; .d.ts ɵcmp ${comparison.dtsAllEqual ? 'EQUAL' : 'DIFFERS'}.`,
		)
	} else if (comparisonError) {
		console.log(`\n  comparison error: ${comparisonError}`)
	}
	console.log(`\nwrote ${outPath}`)
	return out
}

main().catch((err) => {
	console.error('BENCH ERROR:', err)
	process.exit(1)
})
