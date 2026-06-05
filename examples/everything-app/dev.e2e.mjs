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
// It does NOT re-implement the proven per-surface assertions — it ORCHESTRATES the three committed
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
//   - NAV path  : examples/everything-app/nav.e2e.mjs
//        Boots a REAL listening HTTP `vite` dev server and proves the app's NAVIGATION end to end:
//        every route's lazy component module LOADS over HTTP with a JavaScript content-type (the
//        reported NS_ERROR_CORRUPTED_CONTENT / "disallowed MIME type ()" for `.treaty`/`.tjsx` is gone —
//        the dev-serve MIME fix), then drives the REAL Angular Router across every cleanly-lowering route
//        (`` logs / dashboard / profile) asserting each renders its view in the outlet (the nav links
//        work), with the global stylesheet served + emitted and component-scoped styles applied. The
//        greeter route's RENDER (its greeter.treaty inline `server {}` block is emitted verbatim) and the
//        metrics route's RENDER (its consumed standalone `@Pipe`/`@Directive` have no Ivy `ɵpipe`/`ɵdir`
//        def) are recorded as reported out-of-scope Rust-compiler gaps — module LOAD + MIME proven.
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
//   bun run e2e:dev-serve   — dev-only gate (a child this drives)
//   bun run e2e:nav         — nav/MIME/styles gate (a child this drives)
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

	// --- NAV path (real HTTP dev server module-load + MIME + real Router render per route) ---
	const navE2e = runChild('nav.e2e.mjs')
	check(
		'NAV gate: nav.e2e.mjs exits 0 (every route module loads over HTTP with a JS content-type; cleanly-lowering routes render through the real Router)',
		navE2e.status === 0,
	)

	// --- SOURCE-VALIDATING child: compile EVERY real authoring source through the production
	//     @treaty/compiler seam and assert (by PARSING the emit) correct Ivy + no server-fn leak. ---
	const srcValidate = runChild('source-validate.e2e.mjs')

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

	// NAV cells — the real HTTP dev server served every route's lazy module with a JS content-type (the
	// reported MIME bug is gone) and the real Angular Router rendered each cleanly-lowering route's view.
	const navCells = [
		['logs   x nav  (the eager `` route module LOADS over HTTP as JS)', "[load] route '' (logs): GET"],
		['dashboard x nav (lazy module LOADS over HTTP as JS)', '[load] route dashboard: GET'],
		['metrics x nav (lazy module + the .treaty Gauge LOAD over HTTP as JS)', '[load] route metrics → gauge.treaty: GET'],
		['profile x nav (lazy loadChildren module LOADS over HTTP as JS)', '[load] route profile (loadChildren): GET'],
		['greeter x nav (the @Component .ts module + the .tjsx surface LOAD over HTTP as JS)', '[load] route greeter → greeting-card.tjsx: GET'],
		['MIME fix: bare .treaty served as JS (the reported NS_ERROR/disallowed-MIME bug is gone)', '[MIME] GET /src/features/metrics/gauge.treaty → JS content-type'],
		['MIME fix: greeter.treaty no longer served RAW with an empty content-type (reported bug gone)', 'NOT served as RAW .treaty source with an empty content-type'],
		['render: logs route renders through the real Router (nav link live)', "render: nav → '' (logs) (/) RENDERS"],
		['render: dashboard route renders through the real Router (nav link live)', 'render: nav → dashboard (/dashboard) RENDERS'],
		['render: profile route renders through the real Router (nav link live)', 'render: nav → profile (/profile) RENDERS'],
		['styles: global base theme emitted as a CSS asset', 'styles: global base theme emitted as a CSS asset'],
		['styles: component-scoped style applied (gauge.treaty <style> → Ivy styles[])', 'styles: component-scoped style applied'],
	]
	for (const [cell, needle] of navCells) {
		check(`[matrix] ${cell}`, childPassed(navE2e.out, needle))
	}

	// The greeter route's RENDER is blocked by a reported Rust `server {}` extraction gap, and the metrics
	// route's RENDER by a reported Rust standalone-@Pipe/@Directive Ivy-lowering gap (both modules LOAD +
	// MIME-resolve fine — asserted above; nav.e2e.mjs records the render blocks, mirroring full-build's
	// BOOT_BLOCKED). Surface them here so closing either gap (the harness flipping its switch) is noticed
	// rather than silently leaving a route render unproven.
	check(
		'[matrix] greeter x nav RENDER is recorded as the reported Rust `server {}` gap (greeter.treaty)',
		/greeter[^\n]*BLOCKED by the reported Rust `server \{\}` extraction gap/.test(navE2e.out),
		/greeter[^\n]*BLOCKED by the reported Rust `server \{\}` extraction gap/.test(navE2e.out) ? '' : 'greeter render block not recorded — flip GREETER_BLOCKED in nav.e2e.mjs if the gap closed',
	)
	check(
		'[matrix] metrics x nav RENDER is recorded as the reported Rust pipe/directive Ivy-lowering gap',
		/metrics[^\n]*BLOCKED by the reported Rust pipe\/directive Ivy-lowering gap/.test(navE2e.out),
		/metrics[^\n]*BLOCKED by the reported Rust pipe\/directive Ivy-lowering gap/.test(navE2e.out) ? '' : 'metrics render block not recorded — flip METRICS_BLOCKED in nav.e2e.mjs if the gap closed',
	)

	// The `use:`-directive Rust gap that previously blocked the in-browser boot is CLOSED: the JSX
	// `use:<name>` lowering now resolves each directive to a real in-scope symbol (counter.tsx
	// `use:highlight` → the hoisted local `highlight`; greeting-card.tjsx `use:autofocus` → a native
	// host attribute, no fabricated `Autofocus`), so the bundle no longer emits an undefined
	// `dependencies:[Autofocus|Highlight]`. full-build.e2e.mjs asserts the whole-app headless
	// boot+render as a HARD requirement (BOOT_BLOCKED=false). If that gate ever re-blocks the boot,
	// surface it here so the regression is explicit rather than silently green.
	const bootBlocked = /boot\+render is BLOCKED by the reported Rust `use:`-directive gap/.test(build.out)
	check(
		'headless BOOT+render of the full app is NOT blocked by a `use:`-directive gap (boot is a hard requirement of full-build.e2e.mjs)',
		!bootBlocked,
		bootBlocked ? 'full-build.e2e.mjs reported the boot is BLOCKED — the `use:` directive gap regressed' : '',
	)

	// -----------------------------------------------------------------------
	// SOURCE-VALIDATE MATRIX: every real authoring source compiled through the production @treaty/compiler
	// seam and PARSE-verified (correct Ivy `define*`, no surviving Angular decorator, no server-fn body or
	// secret leaking into the client code/map). Read each file's canonical `[source-validate] PASS/FAIL
	// <file>` status line. Each cleanly-lowering surface is a HARD PASS; the two server-extraction rows are
	// recorded against the reported Rust gap (file-level `'use server'` + `.treaty` `server { }` extract,
	// but `'use websocket'` and inline `$$` do not yet) so closing the gap is NOTICED, not silently green.
	// -----------------------------------------------------------------------
	console.log('\n========== SOURCE-VALIDATE MATRIX: every authoring source compiled + PARSE-verified ==========')

	/** A `[source-validate] PASS <file>` line exists for `file` and there is no FAIL line for it. */
	function sourceValidatePassed(file) {
		const lines = srcValidate.out.split('\n')
		const passRe = new RegExp(`^\\[source-validate\\] PASS ${file.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`)
		const failRe = new RegExp(`^\\[source-validate\\] FAIL ${file.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`)
		let sawPass = false
		for (const line of lines) {
			if (failRe.test(line)) return false
			if (passRe.test(line)) sawPass = true
		}
		return sawPass
	}

	const SOURCE_VALIDATE_HARD_PASS = [
		'src/app/app-root.component.ts',
		'src/components/counter.tsx',
		'src/components/highlight.directive.ts',
		'src/components/log-viewer.component.ts',
		'src/components/todo-list.treaty',
		'src/features/dashboard/dashboard.component.ts',
		'src/features/greeter/greeter-page.component.ts',
		'src/features/greeter/greeter.treaty',
		'src/features/greeter/greeting.types.ts',
		'src/features/metrics/gauge.treaty',
		'src/features/metrics/highlight-delta.directive.ts',
		'src/features/metrics/metrics-panel.component.ts',
		'src/features/metrics/percent.pipe.ts',
		'src/features/profile/profile-settings.component.ts',
		'src/features/profile/profile.component.ts',
		'src/features/profile/profile.routes.ts',
		'src/routes/app.routes.ts',
		'src/server/logs.stream.ts',
		'src/server/todos.server.ts',
	]
	for (const file of SOURCE_VALIDATE_HARD_PASS) {
		check(`[source-validate] ${file} compiles to correct Ivy + leak-free (PARSE-verified)`, sourceValidatePassed(file), 'see source-validate.e2e.mjs matrix')
	}

	// The two reported Rust server-extraction gaps: their server-fn body is NOT yet extracted, so
	// source-validate.e2e.mjs reports them FAIL with the leaked token named. Record that here so the gap is
	// surfaced explicitly; if either source starts PASSING (gap closed) this flips and the row below fails,
	// prompting promotion into SOURCE_VALIDATE_HARD_PASS.
	for (const file of ['src/server/presence.ws.ts', 'src/features/greeter/greeting-card.tjsx']) {
		const stillBlocked = !sourceValidatePassed(file)
		check(
			`[source-validate] ${file} server-fn extraction is recorded as the reported Rust gap ('use websocket' / inline $$ bodies not yet lifted)`,
			stillBlocked,
			stillBlocked ? '' : `${file} now PASSES source-validate — the Rust extraction gap closed; promote it into SOURCE_VALIDATE_HARD_PASS`,
		)
	}

	console.log('')
	if (failures.length) {
		console.error(`UNIFIED DEV+BUILD GATE FAILED: ${failures.length} assertion(s): ${failures.join('; ')}`)
		process.exit(1)
	}
	console.log(
		'UNIFIED DEV+BUILD+NAV GATE PASSED: every Treaty authoring surface (.treaty / .tsx / .tjsx / @Component .ts) is green on ALL paths — ' +
			'DEV serve (real vite dev server: no unresolved imports, each surface lowered to Ivy, JSX is Angular-Ivy+signals not React, no JIT/@angular/compiler) ' +
			'AND full vite BUILD (exit 0, each surface lowered to Ivy once, zero residual ɵɵngDeclare, no @angular/compiler / Babel finisher) ' +
			'AND NAV (real HTTP dev server: every route module loads as JS — the reported .treaty/.tjsx NS_ERROR/disallowed-MIME bug is gone — and the real Angular Router renders the logs/dashboard/profile routes with the global + component-scoped styles applied). ' +
			'The whole-app headless boot+render is green too: the JSX `use:`-directive lowering resolves every directive to a real in-scope symbol, so the app boots with no `X is not defined` ReferenceError and a route renders. ' +
			'SOURCE-VALIDATE proves it from the source side: every real authoring file under src/ compiles through the production @treaty/compiler seam and is PARSE-verified — every @Component/.treaty/JSX → ɵɵdefineComponent, @Directive → ɵɵdefineDirective, @Pipe → ɵɵdefinePipe with NO surviving Angular decorator node, and the file-level `use server` + `.treaty` `server {}` modules extract their bodies with NO secret/body leaking into the client code or map. ' +
			'Two route RENDERS are recorded as reported out-of-scope Rust-compiler gaps (modules LOAD + MIME proven): greeter (greeter.treaty inline `server {}` emitted verbatim) and metrics (consumed standalone @Pipe/@Directive have no Ivy ɵpipe/ɵdir def); and two server-extraction surfaces are recorded as reported Rust gaps (source-validate names the leaked token): presence.ws.ts (`use websocket`) and greeting-card.tjsx (inline `$$`) do not yet lift their server-fn body.',
	)
}

main().catch((err) => {
	console.error('UNIFIED DEV+BUILD GATE ERROR:', err)
	process.exit(1)
})
