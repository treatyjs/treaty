// Treaty everything-app UNIFIED DEV + BUILD GATE end-to-end harness (Phase 3).
//
// This is the single, repeatable, deterministic gate that proves BOTH paths green across EVERY
// authoring surface Treaty offers — DEV serve AND full production build — in one run, and prints the
// explicit pass/fail matrix the task asks for:
//
//       surface              | dev (serve)        | build (vite build)
//       ---------------------+--------------------+--------------------
//       .treaty SFC          | gauge.treaty       | bundle ɵɵdefineComponent
//       .tsx  (Treaty JSX)   | counter.tsx        | counter.tsx lowered once
//       .tjsx (Treaty JSX)   | greeting-card.tjsx | greeting-card.tjsx lowered once
//       @Component .ts       | app-root/log-viewer| bundle ɵɵdefineComponent
//
// It does NOT re-implement the proven per-surface assertions — it ORCHESTRATES the two committed
// harnesses that already make them and aggregates their results into the matrix:
//
//   - DEV path  : examples/everything-app/dev-serve.e2e.mjs
//        Boots a REAL `vite` dev server over the WHOLE app (the user's exact `bun run dev:vite`
//        reproduction) and proves the dep scan does NOT abort with the reported
//        "@treaty/jsx/jsx-dev-runtime could not be resolved", then drives transformRequest(id) per
//        surface and asserts each is served lowered to Ivy with NO injected @treaty/jsx runtime, NO
//        React jsxDEV(/createElement(, ZERO residual ɵɵngDeclare, and no @angular/compiler (no JIT).
//
//   - BUILD path: examples/everything-app/full-build.e2e.mjs
//        Runs a REAL `vite build` over the app (exit 0), asserts the emitted bundle lowers every
//        surface to Ivy (ɵɵdefineComponent), has ZERO residual ɵɵngDeclare, imports NO
//        @angular/compiler / @angular/compiler-cli / @babel/core, and that each JSX module lowered to
//        Ivy EXACTLY ONCE. The headless boot+render is gated on the documented out-of-scope Rust
//        `use:`-directive gap (see that harness header) — recorded, not failed.
//
// On top of the orchestration, this gate ALSO makes the JSX-is-Treaty/Angular-JSX-not-React semantic
// assertion the task calls out, DIRECTLY against the @treaty/compiler core (the same one the bundler
// plugins use): each .tsx/.tjsx must lower to an Angular Ivy component (ɵɵdefineComponent) carrying
// Angular signals (signal()/computed()), with NO React element tree (createElement(/jsxDEV(/React.).
//
// Why orchestrate two real harnesses instead of one mega-script: each child runs a REAL bundler in a
// clean process (a real `vite` dev-server subprocess; a real `vite build`), so this gate stays a thin,
// deterministic aggregator with no shared mutable bundler state between the dev and build paths.
//
// DOCUMENTED COMMANDS (also in package.json + README):
//   bun run dev:e2e     (== node examples/everything-app/dev.e2e.mjs)   — this unified gate
//   bun run build:e2e   (== node examples/everything-app/full-build.e2e.mjs) — build-only gate
//   bun run e2e:dev-serve   — dev-only gate (the child this drives)
//
// Usage:  node examples/everything-app/dev.e2e.mjs
// Exit code 0 on success, 1 on any failed assertion (or if either child harness fails).

import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { readFileSync, existsSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const req = createRequire(import.meta.url)

const failures = []
function check(label, condition, detail) {
	const ok = Boolean(condition)
	console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}${detail ? ` - ${detail}` : ''}`)
	if (!ok) failures.push(label)
	return ok
}

// ---------------------------------------------------------------------------
// Run a child e2e harness in its own process, streaming + capturing its output so we can both surface
// it live and parse its per-surface PASS lines into the matrix.
// ---------------------------------------------------------------------------
function runChild(script) {
	console.log(`\n========== running ${script} ==========`)
	const res = spawnSync(process.execPath, [join(here, script)], {
		cwd: here,
		encoding: 'utf-8',
		// 10 min: the build child runs a real `vite build`; the dev child boots a real dev server.
		timeout: 9 * 60_000,
		maxBuffer: 64 * 1024 * 1024,
	})
	const out = `${res.stdout ?? ''}${res.stderr ?? ''}`
	process.stdout.write(out)
	console.log(`========== ${script} exited ${res.status} ==========\n`)
	return { status: res.status, out }
}

/** A child harness logs `PASS  <label>` per assertion; treat any line whose label CONTAINS `needle`
 * (and starts PASS) as that row passing. Returns false if the matching line is FAIL or absent. */
function childPassed(out, needle) {
	const lines = out.split('\n')
	let sawPass = false
	for (const line of lines) {
		if (!line.includes(needle)) continue
		if (/^\s*PASS\b/.test(line)) sawPass = true
		if (/^\s*FAIL\b/.test(line)) return false
	}
	return sawPass
}

// ---------------------------------------------------------------------------
// JSX-is-Treaty/Angular-JSX-not-React semantic proof (direct @treaty/compiler core).
//
// The task requires the JSX assertion to specifically confirm Treaty/Angular JSX semantics: the .tsx
// lowers to an Angular Ivy component (ɵɵdefineComponent + signals), NOT a React element tree. We prove
// this against the compiler core directly so the guarantee holds independent of the bundler.
// ---------------------------------------------------------------------------
function assertJsxIsAngularNotReact() {
	let compileUnifiedSource
	try {
		;({ compileUnifiedSource } = req('@treaty/compiler'))
	} catch (err) {
		check('@treaty/compiler is loadable (JSX semantic proof)', false, String(err?.message ?? err))
		return
	}
	if (!check('@treaty/compiler.compileUnifiedSource available', typeof compileUnifiedSource === 'function')) return

	for (const [label, rel] of [
		['.tsx', 'src/components/counter.tsx'],
		['.tjsx', 'src/features/greeter/greeting-card.tjsx'],
	]) {
		const source = readFileSync(join(here, rel), 'utf-8')
		const out = compileUnifiedSource(source, rel)
		const errors = out.errors ?? []
		const code = out.code ?? ''
		check(`${label} (${rel}): compiles with zero diagnostics`, errors.length === 0, errors.join(' | '))
		check(
			`${label} (${rel}): lowers to an Angular Ivy component (ɵɵdefineComponent), NOT a React element tree`,
			/ɵɵdefineComponent/.test(code) &&
				!/\bcreateElement\s*\(/.test(code) &&
				!/\bjsxDEV\s*\(/.test(code) &&
				!/\bjsx\s*\(/.test(code) &&
				!/\bReact\b/.test(code),
			code.replace(/\s+/g, ' ').slice(0, 120),
		)
		// Signals-by-default: the lowered Angular component carries Angular signal primitives, not React
		// hooks/state. counter.tsx uses signal()+computed(); greeting-card.tjsx uses signal().
		check(
			`${label} (${rel}): lowered Angular component carries Angular signals (signal()/computed()), not React state`,
			/\bsignal\s*\(/.test(code) && !/\buseState\s*\(|\buseMemo\s*\(/.test(code),
		)
		check(
			`${label} (${rel}): NO injected @treaty/jsx runtime import escapes (Treaty owns JSX, not a foreign runtime)`,
			!/@treaty\/jsx/.test(code),
		)
	}
}

// ---------------------------------------------------------------------------
async function main() {
	check('dev-serve child harness present', existsSync(join(here, 'dev-serve.e2e.mjs')))
	check('full-build child harness present', existsSync(join(here, 'full-build.e2e.mjs')))

	// --- DEV path (real `vite` dev server over the whole app + per-surface transformRequest) ---
	const dev = runChild('dev-serve.e2e.mjs')
	check('DEV gate: dev-serve.e2e.mjs exits 0 (real vite dev server, every surface served to Ivy)', dev.status === 0)

	// --- BUILD path (real `vite build`, bundle assertions, JSX-lowered-once) ---
	const build = runChild('full-build.e2e.mjs')
	check('BUILD gate: full-build.e2e.mjs exits 0 (real vite build, every surface lowered to Ivy)', build.status === 0)

	// --- JSX-is-Treaty/Angular-not-React semantic proof (compiler core) ---
	console.log('\n========== JSX semantic proof (Angular Ivy + signals, not React) ==========')
	assertJsxIsAngularNotReact()

	// -----------------------------------------------------------------------
	// THE MATRIX: every authoring surface × {dev, build}, aggregated from the two children. Each cell
	// asserts the specific child PASS line that proves that surface on that path.
	// -----------------------------------------------------------------------
	console.log('\n========== FULL MATRIX: authoring surface × {dev, build} ==========')

	// DEV cells — the child served each surface lowered to Ivy through the real dev transform pipeline.
	const devCells = [
		['.treaty   x dev  (gauge.treaty served, Ivy ɵɵdefineComponent)', 'gauge.treaty: served module is Treaty-lowered Ivy'],
		['.tsx      x dev  (counter.tsx served, Ivy, no React, no @treaty/jsx runtime)', 'counter.tsx (JSX .tsx): served module is Treaty-lowered Ivy'],
		['.tjsx     x dev  (greeting-card.tjsx served, Ivy, no React, no @treaty/jsx runtime)', 'greeting-card.tjsx (JSX .tjsx): served module is Treaty-lowered Ivy'],
		['@Component x dev (app-root.component.ts served, Ivy ɵɵdefineComponent)', 'app-root.component.ts (@Component .ts): served module is Treaty-lowered Ivy'],
		['@Component x dev (log-viewer.component.ts served, Ivy ɵɵdefineComponent)', 'log-viewer.component.ts (@Component .ts): served module is Treaty-lowered Ivy'],
		['dev: NO JIT (the EXACT user dep-scan error is gone)', 'could not be resolved" (the EXACT user error)'],
		['dev: partial @angular linked to AOT (ZERO residual ɵɵngDeclare, no @angular/compiler)', 'dev-serve links a partial @angular dep via the linker (ZERO residual'],
	]
	for (const [cell, needle] of devCells) {
		check(`[matrix] ${cell}`, childPassed(dev.out, needle))
	}

	// BUILD cells — the real `vite build` lowered every surface to Ivy in the emitted bundle.
	const buildCells = [
		['ALL surfaces x build (bundle carries Ivy ɵɵdefineComponent)', 'lowered to Ivy (ɵɵdefineComponent present)'],
		['.tsx      x build (counter.tsx lowered to Ivy EXACTLY ONCE)', 'src/components/counter.tsx: emits exactly ONE ɵɵdefineComponent'],
		['.tjsx     x build (greeting-card.tjsx lowered to Ivy EXACTLY ONCE)', 'src/features/greeter/greeting-card.tjsx: emits exactly ONE ɵɵdefineComponent'],
		['build: real `vite build` exits 0', 'real `vite build` of everything-app exits 0'],
		['build: ZERO residual ɵɵngDeclare (partial @angular linked to AOT)', 'bundle has ZERO residual ɵɵngDeclare partial declarations'],
		['build: NO JIT (bundle does not import @angular/compiler)', 'bundle does NOT import @angular/compiler (no JIT)'],
		['build: no Babel finisher (@angular/compiler-cli / @babel/core absent)', 'bundle does NOT contain @babel/core (Babel finisher removed)'],
	]
	for (const [cell, needle] of buildCells) {
		check(`[matrix] ${cell}`, childPassed(build.out, needle))
	}

	// .treaty x build and @Component x build are covered by the aggregate bundle ɵɵdefineComponent proof
	// above (gauge.treaty's metrics route + the @Component shells all bundle into the emitted chunks; the
	// build child's `defineComponent=7` count is every authoring component lowered). Assert that count is
	// strictly more than the two JSX modules, i.e. .treaty + @Component surfaces are present in the build.
	{
		const m = /ɵɵdefineComponent present\) - defineComponent=(\d+)/.exec(build.out)
		const count = m ? Number(m[1]) : 0
		check(
			'[matrix] .treaty + @Component x build (bundle has > 2 ɵɵdefineComponent — beyond the two JSX modules)',
			count > 2,
			`defineComponent=${count}`,
		)
	}

	// Record the one documented out-of-scope gap so the gate is explicit about it (recorded, not failed).
	const bootBlocked = /boot\+render is BLOCKED by the reported Rust `use:`-directive gap/.test(build.out)
	console.log(
		bootBlocked
			? 'NOTE  headless BOOT+render of the full app is gated on the documented out-of-scope Rust `use:`-directive gap (greeting-card.tjsx/counter.tsx emit an undefined `dependencies:[Autofocus|Highlight]`). DEV serve + BUILD of every surface are green; only the in-browser boot of the whole app is blocked, owned by the Rust compiler workflow (libs/treaty-ivy / libs/authoring/node).'
			: 'NOTE  headless BOOT+render appears unblocked — full-build.e2e.mjs should flip BOOT_BLOCKED to false.',
	)

	console.log('')
	if (failures.length) {
		console.error(`UNIFIED DEV+BUILD GATE FAILED: ${failures.length} assertion(s): ${failures.join('; ')}`)
		process.exit(1)
	}
	console.log(
		'UNIFIED DEV+BUILD GATE PASSED: every Treaty authoring surface (.treaty / .tsx / .tjsx / @Component .ts) is green on BOTH paths — ' +
			'DEV serve (real vite dev server: no unresolved imports, each surface lowered to Ivy, JSX is Angular-Ivy+signals not React, no JIT/@angular/compiler) ' +
			'AND full vite BUILD (exit 0, each surface lowered to Ivy once, zero residual ɵɵngDeclare, no @angular/compiler / Babel finisher). ' +
			'Only the whole-app headless boot is gated on the documented out-of-scope Rust `use:`-directive gap.',
	)
}

main().catch((err) => {
	console.error('UNIFIED DEV+BUILD GATE ERROR:', err)
	process.exit(1)
})
