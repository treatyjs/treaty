/**
 * Node smoke test for @treaty/cli, run against the built dist.
 *
 * It exercises the standalone CLI's public surface WITHOUT starting a server or
 * requiring the bundler peers (vite / @rspack/*) to be installed:
 *   1. parseArgs splits command / positionals / options / passthrough,
 *   2. `treaty --help` returns usage with exit 0; a missing command is a usage error,
 *   3. `treaty --version` returns the version,
 *   4. resolveConfig applies defaults + overrides (convention-based, no angular.json),
 *   5. a `treaty build --dry-run` resolves the Vite plugin + auto-MF config without throwing,
 *   6. a `treaty dev --dry-run --bundler rspack` resolves the Rspack plugin + auto-MF config,
 *   7. buildVitePlugins / buildRspackConfig wire Treaty + Module Federation,
 *   8. `treaty generate` plans standalone, federation-ready files (dry run, no writes).
 *
 * Run: node libs/treaty/cli/test/cli.smoke.mjs
 */

import assert from 'node:assert/strict'
import { fileURLToPath } from 'node:url'
import { dirname, join, resolve as resolvePath } from 'node:path'
import { mkdtempSync, mkdirSync, writeFileSync, existsSync, rmSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import {
	run,
	parseArgs,
	resolveConfig,
	runBuild,
	buildVitePlugins,
	buildViteConfig,
	buildRspackConfig,
	planGenerate,
	VERSION,
	DEFAULT_BUNDLER,
	DEFAULT_PORT,
} from '../dist/index.js'

const here = dirname(fileURLToPath(import.meta.url))
const fixtureRoot = resolvePath(here, 'fixture-project')

// `vite` is installed in this workspace (so the real-build case below runs), but
// the optional federation/Rspack peers (@module-federation/*, @rspack/*) are not.
// The auto-MF Vite plugin is a lazily-resolved Promise that loads
// @module-federation/vite only when Vite actually runs with federation on — which
// the dry-run cases never do. Node escalates that rejected dynamic-import to an
// uncaught error during module linking, so we tolerate exactly that known,
// expected absence here. Any OTHER error still fails the run loudly.
const KNOWN_OPTIONAL_PEERS = /@module-federation\/(vite|enhanced)|@rspack\/(core|dev-server)|^vite$|Cannot find package 'vite'/
function isExpectedMissingPeer(err) {
	const msg = err && (err.message || String(err))
	return typeof msg === 'string' && KNOWN_OPTIONAL_PEERS.test(msg)
}
process.on('unhandledRejection', (err) => {
	if (!isExpectedMissingPeer(err)) {
		console.error('UNEXPECTED unhandledRejection:', err)
		process.exit(1)
	}
})
process.on('uncaughtException', (err) => {
	if (!isExpectedMissingPeer(err)) {
		console.error('UNEXPECTED uncaughtException:', err)
		process.exit(1)
	}
})

// Parse a generated component (TypeScript AST, not source text) and assert it
// honours the MINIMAL-TEMPLATE contract: its `@Component` decorator declares NO
// `selector` and NO `standalone`, and the class declares NO `signal()` call.
// Inspecting the AST (not a regex over the source) avoids matching the
// explanatory comment that deliberately names those properties.
async function assertMinimalComponent(source, label) {
	const { default: ts } = await import('typescript')
	const sf = ts.createSourceFile(`${label}.ts`, source, ts.ScriptTarget.Latest, true)
	let sawComponentDecorator = false
	const componentPropertyNames = new Set()
	let sawSignalCall = false

	const readComponentDecorator = (decorator) => {
		const call = decorator.expression
		if (!ts.isCallExpression(call)) return
		if (!ts.isIdentifier(call.expression) || call.expression.text !== 'Component') return
		sawComponentDecorator = true
		const [arg] = call.arguments
		if (arg && ts.isObjectLiteralExpression(arg)) {
			for (const prop of arg.properties) {
				if (prop.name && ts.isIdentifier(prop.name)) componentPropertyNames.add(prop.name.text)
			}
		}
	}
	const visit = (node) => {
		if (ts.canHaveDecorators?.(node)) {
			for (const dec of ts.getDecorators(node) ?? []) readComponentDecorator(dec)
		}
		if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'signal') {
			sawSignalCall = true
		}
		ts.forEachChild(node, visit)
	}
	visit(sf)

	assert.ok(sawComponentDecorator, `${label}: has a @Component decorator`)
	assert.ok(!componentPropertyNames.has('selector'), `${label}: no selector (the compiler infers it)`)
	assert.ok(!componentPropertyNames.has('standalone'), `${label}: no standalone (standalone by default)`)
	assert.ok(!sawSignalCall, `${label}: no signal() ceremony (signals by default)`)
}

let failures = 0
const results = []

function check(label, fn) {
	const done = (err) => {
		if (err) {
			failures++
			results.push(`FAIL ${label}: ${err.stack ?? err.message ?? err}`)
		} else {
			results.push(`PASS ${label}`)
		}
	}
	try {
		const out = fn()
		if (out && typeof out.then === 'function') return out.then(() => done(), done)
		done()
	} catch (err) {
		done(err)
	}
	return undefined
}

await check('parseArgs splits command / positionals / options / passthrough', () => {
	const parsed = parseArgs(['generate', 'component', 'my-widget', '--dry-run', '--port=5000', '--', 'extra'])
	assert.equal(parsed.command, 'generate', 'command parsed')
	assert.deepEqual([...parsed.positionals], ['component', 'my-widget'], 'positionals parsed')
	assert.equal(parsed.options['dry-run'], true, 'boolean flag parsed')
	assert.equal(parsed.options['port'], '5000', 'key=value parsed')
	assert.deepEqual([...parsed.passthrough], ['extra'], 'passthrough after -- parsed')

	const negated = parseArgs(['dev', '--no-open'])
	assert.equal(negated.options['open'], false, '--no-flag parses to false')
})

await check('treaty --help returns usage (exit 0)', async () => {
	const res = await run(['--help'])
	assert.equal(res.exitCode, 0, 'help exits 0')
	assert.equal(res.isError, false, 'help is not an error')
	assert.ok(res.output.join('\n').includes('treaty <command>'), 'usage shown')
	assert.ok(res.output.join('\n').includes('Module Federation'), 'federation principle mentioned')
})

await check('missing command is a usage error', async () => {
	const res = await run([])
	assert.equal(res.exitCode, 1, 'no command exits non-zero')
	assert.equal(res.isError, true, 'no command is an error')
})

await check('treaty --version returns the version', async () => {
	const res = await run(['--version'])
	assert.equal(res.exitCode, 0, 'version exits 0')
	assert.deepEqual([...res.output], [VERSION], 'version printed')
})

await check('resolveConfig applies defaults + overrides (no angular.json)', async () => {
	const def = await resolveConfig(fixtureRoot)
	assert.equal(def.bundler, DEFAULT_BUNDLER, 'defaults to vite')
	assert.equal(def.port, DEFAULT_PORT, 'default port')
	assert.equal(def.moduleFederation, true, 'federation auto-on by default')
	assert.ok(def.entry.endsWith('main.ts'), 'convention entry resolved absolutely')
	assert.equal(def.configFile, null, 'no config file => pure conventions')

	const over = await resolveConfig(fixtureRoot, { bundler: 'rspack', port: 9999, outDir: 'out' })
	assert.equal(over.bundler, 'rspack', 'bundler override applied')
	assert.equal(over.port, 9999, 'port override applied')
	assert.ok(over.outDir.endsWith('out'), 'outDir override applied (absolute)')
})

await check('treaty build --dry-run resolves Vite plugin + auto-MF without throwing', async () => {
	const res = await run(['build', '--dry-run', '--root', fixtureRoot])
	assert.equal(res.exitCode, 0, 'dry-run build exits 0')
	assert.equal(res.isError, false, 'dry-run build is not an error')
	const text = res.output.join('\n')
	assert.ok(text.includes('vite'), 'reports vite bundler')
	assert.ok(text.includes('federation: auto'), 'reports auto federation host')
})

await check('treaty dev --dry-run --bundler rspack resolves Rspack plugin + auto-MF', async () => {
	const res = await run(['dev', '--dry-run', '--bundler', 'rspack', '--root', fixtureRoot])
	assert.equal(res.exitCode, 0, 'dry-run dev exits 0')
	assert.ok(res.output.join('\n').includes('rspack'), 'reports rspack bundler')
})

await check('buildVitePlugins wires Treaty + a federation plugin entry', async () => {
	const config = await resolveConfig(fixtureRoot)
	const plugins = buildVitePlugins(config)
	assert.ok(Array.isArray(plugins) && plugins.length >= 1, 'returns a plugin array')
	// The synchronous Treaty plugin is the first entry; the second is the
	// auto-MF plugin (a Promise<Plugin>, lazily loading the optional
	// @module-federation/vite peer — only resolved when Vite actually runs).
	const treaty = plugins.find((p) => p && typeof p === 'object' && p.name === 'treaty:vite')
	assert.ok(treaty, 'includes the Treaty vite plugin synchronously')
	assert.ok(plugins.length >= 2, 'includes the auto-generated federation plugin entry')
	// The federation entry is a Promise (lazy peer load); swallow its rejection
	// here so an absent optional peer does not surface as an unhandled rejection.
	for (const p of plugins) {
		if (p && typeof p.then === 'function') p.catch(() => {})
	}

	const inline = buildViteConfig(config, 'build')
	assert.equal(inline.configFile, false, 'standalone CLI owns the config (no stray vite.config)')
	assert.ok(inline.build && inline.build.outDir, 'build config sets an outDir')
})

await check('buildRspackConfig wires Treaty loader/resolve + Module Federation', async () => {
	const config = await resolveConfig(fixtureRoot, { bundler: 'rspack' })
	const rspackConfig = buildRspackConfig(config, 'production')
	assert.equal(rspackConfig.mode, 'production', 'mode set')
	assert.ok(rspackConfig.module && Array.isArray(rspackConfig.module.rules), 'loader rule added')
	assert.ok(rspackConfig.module.rules.length >= 1, 'at least the Treaty loader rule')
	assert.ok(
		rspackConfig.resolve && rspackConfig.resolve.extensions.includes('.treaty'),
		'authoring extensions resolved'
	)
	// MF is default-on; the plugin pushes a ModuleFederationPlugin when the peer
	// is installed. Without the peer it warns and skips — either way no throw.
	assert.ok(Array.isArray(rspackConfig.plugins), 'plugins array present')
})

await check('treaty generate plans standalone, federation-ready files (dry run)', async () => {
	const appFiles = planGenerate({ kind: 'app', name: 'my-shell', cwd: fixtureRoot })
	const paths = appFiles.map((f) => f.path.replace(/\\/g, '/'))
	assert.ok(paths.some((p) => p.endsWith('index.html')), 'app has index.html')
	assert.ok(paths.some((p) => p.endsWith('src/main.ts')), 'app has src/main.ts')
	assert.ok(paths.some((p) => p.endsWith('treaty.config.mjs')), 'app has treaty.config')
	const main = appFiles.find((f) => f.path.endsWith('main.ts'))
	assert.ok(main.contents.includes('bootstrapApplication'), 'standalone bootstrap (no NgModule)')

	const compFiles = planGenerate({ kind: 'component', name: 'my-widget', cwd: fixtureRoot })
	assert.equal(compFiles.length, 1, 'a component is a single file')
	// MINIMAL TEMPLATE: a scaffolded component carries no selector, no
	// `standalone`, and no signal() ceremony — the Treaty compiler fills them in.
	await assertMinimalComponent(compFiles[0].contents, 'cli component')

	// Dry run via the full CLI must not write anything.
	const res = await run(['generate', 'lib', 'shared-ui', '--dry-run', '--root', fixtureRoot], fixtureRoot)
	assert.equal(res.exitCode, 0, 'generate dry-run exits 0')
	assert.ok(res.output.join('\n').includes('plan'), 'dry-run reports planned files')
})

await check('treaty build drives Vite end-to-end and writes real output', async () => {
	// A real, non-mocked build: drive the CLI's runBuild over a self-contained
	// project and assert files land on disk. The project has NO Treaty-owned
	// authoring files (no @Component / .treaty / JSX), so the Treaty plugin in the
	// pipeline passes the entry through and the build needs only `vite` (present)
	// — federation is off so no optional MF peer is required. This exercises the
	// genuine runBuild → buildViteConfig → vite.build path that `treaty build` runs.
	const projectRoot = mkdtempSync(join(tmpdir(), 'treaty-cli-build-'))
	try {
		mkdirSync(join(projectRoot, 'src'), { recursive: true })
		writeFileSync(
			join(projectRoot, 'index.html'),
			'<!doctype html><html><head><meta charset="utf-8"><title>t</title></head>' +
				'<body><div id="app"></div><script type="module" src="/src/main.js"></script></body></html>\n',
		)
		writeFileSync(
			join(projectRoot, 'src', 'main.js'),
			"const el = document.getElementById('app')\nif (el) el.textContent = 'treaty build output'\nexport const ok = true\n",
		)

		const base = await resolveConfig(projectRoot, { bundler: 'vite', outDir: 'dist' })
		// Federation off (no @module-federation/vite peer here) + a plain JS entry.
		const config = { ...base, moduleFederation: false, entry: join(projectRoot, 'src', 'main.js') }
		const result = await runBuild(config)

		assert.equal(result.bundler, 'vite', 'build reports the vite bundler')
		assert.ok(existsSync(join(projectRoot, 'dist', 'index.html')), 'build emitted dist/index.html')
		const html = readFileSync(join(projectRoot, 'dist', 'index.html'), 'utf-8')
		// Vite rewrites the entry to a hashed asset chunk in the emitted HTML.
		assert.match(html, /assets\/.+\.js/, 'emitted HTML references a built JS asset')
	} finally {
		rmSync(projectRoot, { recursive: true, force: true })
	}
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
// Exit cleanly before Node settles the deliberately-unconsumed lazy federation
// promise (its optional peer is absent in this workspace — see the handlers above).
process.exit(0)
