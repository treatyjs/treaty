/**
 * The REPL "everything" demo — one runnable pipeline that exercises EVERY Treaty
 * authoring surface and then takes the result through the full D2
 * deployment-granularity stack (host + remotes, versioned manifest,
 * build-to-deploy, single-entry rollback, runtime resolution).
 *
 * It is intentionally a plain ESM module exporting one function,
 * {@link runEverythingDemo}, with no DOM and no bundler of its own, so it can be
 * driven two ways from the same code path:
 *   - headlessly by `everything-demo.smoke.mjs` (a real build/boot assertion, not
 *     a stub), and
 *   - live by the REPL UI, which renders the returned report.
 *
 * What it demonstrates, end to end:
 *
 *   1. AUTHORING — every surface, lowered to Ivy through `@treaty/compiler`, the
 *      same `transform`/`transformMany` seam every Treaty BUNDLER PLUGIN
 *      (`@treaty/vite`, `@treaty/rspack`) builds on. The surfaces:
 *        - a `.treaty` SFC carrying a top-of-file MACRO block (compile-time,
 *          RSC/Astro style — recognized, parsed, and lifted to a `$macro`
 *          binding), SIGNALS, `@if`/`@for` CONTROL FLOW, and a `server { ... }`
 *          SERVER FN (extracted to its own loadable chunk);
 *        - a `.tjsx` JSX component (lowercase fn component, `@if`, signals);
 *        - an Angular `@Component` `.ts` with a `'use server'` SERVER FN (a second
 *          server-fn transport), so the extracted server module / per-fn chunks
 *          are exercised by a different marker than the `.treaty` one.
 *
 *   2. DEPLOYMENT GRANULARITY (D2) — each compiled feature becomes a lazy route in
 *      a host app's route graph, and the route-graph pass auto-derives the
 *      federated MODULES (host + one remote per feature route + a shared lib) with
 *      NO hand-written exposes. From that graph the demo:
 *        - builds a VITE federation config (the bundler-plugin federation seam) so
 *          the host consumes each feature as a remote;
 *        - builds a VERSIONED MANIFEST (moduleId -> version -> url) and an
 *          operational deployment ledger;
 *        - runs BUILD-TO-DEPLOY: assembles a deploy artifact from a built dir and
 *          uploads it to a pluggable target (the reference FS target);
 *        - performs a single-entry ROLLBACK of one remote and proves the
 *          `@module-federation/enhanced` RUNTIME PLUGIN resolves the rolled-back
 *          url+version at load while every other remote is untouched.
 *
 * The function returns a structured report (every compiled surface + every
 * deployment fact) so the caller asserts on it; it throws on any failure.
 */

import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'

import { classify, createTreatyCompiler } from '@treaty/compiler'
import {
	federatedModuleInputs,
	federatedModules,
	toViteFederation,
} from '@treaty/module-federation'
import {
	buildManifest,
	createDeploymentManifest,
	createTreatyDeploymentRuntimePlugin,
	createTreatyMfRuntimePlugin,
	getModule,
	getRemote,
	recordDeployment,
} from '@treaty/federation-deploy'
import {
	artifactPaths,
	assembleDeployArtifact,
	deploy,
	FsDeployTarget,
	rollback,
} from '@treaty/deploy'

/** @typedef {import('@treaty/compiler').TreatyCompiler} TreatyCompiler */
/** @typedef {import('@treaty/compiler').TreatyFileKind} TreatyFileKind */
/** @typedef {import('@treaty/compiler').ServerFnChunk} ServerFnChunk */
/** @typedef {import('@treaty/module-federation').FederatedModule} FederatedModule */
/** @typedef {import('@treaty/module-federation').MfOptions} MfOptions */
/** @typedef {import('@treaty/federation-deploy').RemoteDeployment} RemoteDeployment */

/**
 * One compiled authoring surface, shaped for the report.
 * @typedef {object} CompiledSurface
 * @property {string} moduleId  Stable federated-module id this feature deploys under.
 * @property {string} feature   Short feature/route name.
 * @property {string} id        Bundler id (extension routes it to an authoring plugin).
 * @property {string} surface   Human-facing description of the authoring surface.
 * @property {TreatyFileKind} kind  The authoring plugin that owns the file.
 * @property {string} code      Emitted Ivy JavaScript.
 * @property {number} codeLength  Length of the emitted code.
 * @property {string|undefined} serverModule  Extracted server module, when any.
 * @property {readonly ServerFnChunk[]} serverChunks  Per-fn extracted server chunks.
 * @property {boolean} sideEffects  Tree-shaking hint.
 */

/**
 * The structured report {@link runEverythingDemo} returns.
 * @typedef {object} EverythingDemoReport
 * @property {string} host
 * @property {CompiledSurface[]} authoring
 * @property {{ moduleIds: string[], modules: FederatedModule[], viteRemotesCount: number, viteName: string }} federation
 * @property {import('@treaty/federation-deploy').FederationManifest} manifest
 * @property {{ partial: boolean, uploadedFiles: string[], publishedManifest: import('@treaty/federation-deploy').FederationManifest }} deploy
 * @property {{ target: string, beforeRollback: RemoteDeployment|undefined, rolledBackEntry: import('@treaty/federation-deploy').ModuleDeployment|undefined, untouched: boolean, runtimeResolvedEntry: string|undefined, runtimeResolvedVersion: string|undefined, ledgerResolvedEntry: string|undefined, ledgerResolvedVersion: string|undefined }} rollback
 */

/**
 * Every authoring surface, one sample each. Each sample is keyed by the bundler
 * `id` whose extension routes it to a specific authoring plugin (exactly how a
 * Treaty bundler plugin keys a module), plus the moduleId this feature deploys
 * under once it is a federated remote.
 *
 * Defaults Treaty fills in (standalone, signal, selectorless, OnPush) are
 * intentionally omitted from the sources — the compiler supplies them.
 */
export const AUTHORING_SAMPLES = [
	{
		moduleId: './routes/macro-panel',
		feature: 'macro-panel',
		id: 'MacroPanel.treaty',
		surface: '.treaty SFC (macro + signals + control flow + server fn)',
		// A top-of-file ``` fence is a COMPILE-TIME MACRO (RSC/Astro style): the
		// compiler recognizes it, parses its body, and lifts it to a compile-time
		// `$macro` binding (compiling the fence away). Combined here with signals, a
		// `server { }` server fn, and @if/@for control flow in one SFC.
		code: [
			'```',
			'// compile-time macro: precompute a static palette, inlined as constants',
			"const palette = ['#6d28d9', '#2563eb', '#059669']",
			'const accents = palette.map((hex, i) => ({ hex, tier: i + 1 }))',
			'const macroMeta = { accents }',
			'```',
			'',
			"import { signal } from '@angular/core'",
			'',
			'server {',
			'  async function loadAccentCount(): Promise<{ count: number }> {',
			'    return { count: 3 }',
			'  }',
			'}',
			'',
			'const status = signal("idle")',
			'async function refresh() {',
			'  const r = await loadAccentCount()',
			'  status.set(`accents: ${r.count}`)',
			'}',
			'',
			'<section class="macro-panel">',
			'  <button (click)="refresh()">{{ status() }}</button>',
			'  @if (status() !== "idle") {',
			'    <ul>',
			'      @for (a of macroMeta.accents; track a.tier) {',
			'        <li [style.color]="a.hex">tier {{ a.tier }}</li>',
			'      }',
			'    </ul>',
			'  }',
			'</section>',
		].join('\n'),
	},
	{
		moduleId: './routes/greeting',
		feature: 'greeting',
		id: 'Greeting.tjsx',
		surface: 'JSX (.tjsx) component (signals + control flow)',
		// Lowercase fn component, signals-by-default input(), Angular @if control flow.
		code: [
			"import { input } from '@angular/core'",
			'',
			'export default function Greeting() {',
			"  const name = input('world')",
			'  return (',
			'    <p class="greeting">',
			'      @if (name()) { Hello, {name()}! } @else { Hello! }',
			'    </p>',
			'  )',
			'}',
		].join('\n'),
	},
	{
		moduleId: './routes/save-note',
		feature: 'save-note',
		id: 'save-note.component.ts',
		surface: "Angular @Component .ts ('use server' server fn)",
		// Classic @Component with a 'use server' fn — a different server-fn marker
		// than the .treaty `server { }` block, both feeding the same extraction.
		code: [
			"import { Component, signal } from '@angular/core'",
			'',
			'export async function saveNote(text: string) {',
			"  'use server'",
			'  return { saved: text.length }',
			'}',
			'',
			'@Component({',
			"  selector: 'save-note',",
			"  template: `<button (click)=\"save()\">Save</button>`,",
			'})',
			'export class SaveNoteComponent {',
			"  readonly draft = signal('')",
			'  async save() { await saveNote(this.draft()) }',
			'}',
		].join('\n'),
	},
]

/** The shared lib every feature route depends on — itself a federated module. */
const SHARED_LIB = './libs/ui-kit'

/** The host container app name. */
const HOST_NAME = 'repl-everything'

/**
 * Compile every authoring sample through the framework-agnostic compiler core —
 * the same seam a Treaty bundler plugin uses — and shape each into a report row.
 * Throws if any owned surface fails to lower, or if a surface is not recognized
 * by the compiler's own routing.
 *
 * @param {TreatyCompiler} compiler
 * @returns {CompiledSurface[]}
 */
function compileAuthoringSurfaces(compiler) {
	return AUTHORING_SAMPLES.map((sample) => {
		const kind = classify(sample.id)
		if (kind === null) {
			throw new Error(`compiler does not own authoring surface ${sample.id}`)
		}
		const result = compiler.transform(sample.id, sample.code)
		if (result === null) {
			throw new Error(`compiler returned no result for owned surface ${sample.id}`)
		}
		return {
			moduleId: sample.moduleId,
			feature: sample.feature,
			id: sample.id,
			surface: sample.surface,
			kind,
			code: result.code,
			codeLength: result.code.length,
			serverModule: result.serverModule,
			serverChunks: result.serverChunks ?? [],
			sideEffects: result.sideEffects,
		}
	})
}

/**
 * Build the host app's route graph from the compiled features: each feature is a
 * LAZY route (so it auto-becomes a deployable remote), plus one eager route (so we
 * prove an eager route is NOT a module) and a shared lib.
 *
 * @param {CompiledSurface[]} features
 * @returns {MfOptions}
 */
function routeGraphFor(features) {
	return {
		name: HOST_NAME,
		routes: [
			{ path: '', redirectTo: features[0].feature, pathMatch: 'full' }, // eager: not a module
			{ path: 'about', component: {} }, // eager component: not a module
			...features.map((f) => ({ path: f.feature, loadComponent: () => ({}) })), // lazy: each a remote
		],
		libs: [SHARED_LIB],
	}
}

/**
 * Look up a module in a manifest, asserting it is present (every moduleId here is
 * derived from the same graph the manifest was built from, so absence is a bug).
 * @param {import('@treaty/federation-deploy').FederationManifest} manifest
 * @param {string} id
 * @returns {import('@treaty/federation-deploy').ModuleDeployment}
 */
function requireModule(manifest, id) {
	const dep = getModule(manifest, id)
	if (dep === undefined) {
		throw new Error(`manifest is missing expected module ${id}`)
	}
	return dep
}

/**
 * Lay down a fixture built dir: <buildDir>/<moduleId>/<files...>.
 * @param {string} buildDir
 * @param {Record<string, Record<string, string>>} layout
 */
async function writeBuild(buildDir, layout) {
	for (const [moduleId, files] of Object.entries(layout)) {
		for (const [rel, contents] of Object.entries(files)) {
			const dest = join(buildDir, moduleId, rel)
			await mkdir(dirname(dest), { recursive: true })
			await writeFile(dest, contents)
		}
	}
}

/**
 * Run the whole everything-demo and return a structured report. Compiles every
 * authoring surface, derives the federated module graph, builds the versioned
 * manifest + deployment ledger, assembles and deploys a real artifact to an FS
 * target, then rolls one remote back and proves the runtime resolves the flip.
 *
 * @returns {Promise<EverythingDemoReport>} a report the caller asserts on.
 */
export async function runEverythingDemo() {
	// 1) AUTHORING — every surface lowered to Ivy through the bundler-plugin seam.
	const compiler = createTreatyCompiler({ cache: true })
	const features = compileAuthoringSurfaces(compiler)

	// 2) DEPLOYMENT GRANULARITY (D2) — the compiled features become a route graph.
	const appGraph = routeGraphFor(features)

	// 2a) route-graph pass: auto-derive the federated modules (NO manual exposes).
	const modules = federatedModules(appGraph)
	const moduleIds = modules.map((m) => m.moduleId)

	// 2b) the bundler-plugin federation seam: a Vite federation config for the host.
	const viteFederation = toViteFederation(appGraph)

	// 2c) a versioned manifest (moduleId -> version -> url), generated from the graph.
	const version = '1.0.0'
	const manifest = buildManifest(
		federatedModuleInputs(appGraph, {
			version,
			urlFor: (m, v) => `https://cdn.example/${m.moduleId}/${v}/remoteEntry.js`,
		}),
		{ app: HOST_NAME }
	)

	// 2d) build-to-deploy: assemble a deploy artifact from a built dir + the manifest,
	// then upload it to a pluggable target (the reference self-hosted FS target).
	const buildDir = await mkdtemp(join(tmpdir(), 'repl-everything-build-'))
	const outDir = await mkdtemp(join(tmpdir(), 'repl-everything-out-'))
	/** @type {import('@treaty/deploy').DeployResult} */
	let deployResult
	/** @type {string[]} */
	let uploadedFiles
	try {
		/** @type {Record<string, Record<string, string>>} */
		const buildLayout = {}
		for (const m of modules) {
			buildLayout[m.moduleId] = {
				'remoteEntry.js': `// ${m.moduleId} remote entry (${m.kind})`,
			}
		}
		await writeBuild(buildDir, buildLayout)

		const artifact = await assembleDeployArtifact({ buildDir, manifest, target: 'fs' })
		const target = new FsDeployTarget({ root: outDir, baseUrl: 'https://cdn.example' })
		deployResult = await deploy(artifact, target)

		// Prove every planned file actually landed on the target.
		uploadedFiles = artifactPaths(artifact)
		for (const path of uploadedFiles) {
			const onDisk = await readFile(join(outDir, path), 'utf8')
			if (onDisk.length === 0) {
				throw new Error(`deploy target produced an empty file at ${path}`)
			}
		}
	} finally {
		await rm(buildDir, { recursive: true, force: true })
		await rm(outDir, { recursive: true, force: true })
	}

	// 2e) operational deployment ledger + a NEW version of one remote, to roll back.
	const rollbackTarget = './routes/greeting'
	let ledger = createDeploymentManifest({ app: HOST_NAME })
	// First, every remote is deployed at 1.0.0 (the live release).
	for (const m of modules) {
		ledger = recordDeployment(
			ledger,
			m.moduleId,
			version,
			requireModule(deployResult.manifest, m.moduleId).url,
			{ kind: m.kind }
		)
	}
	// Then roll ONE remote forward to 2.0.0 — the deploy we are about to roll back.
	ledger = recordDeployment(
		ledger,
		rollbackTarget,
		'2.0.0',
		`https://cdn.example/${rollbackTarget}/2.0.0/remoteEntry.js`,
		{ kind: 'route' }
	)
	const beforeRollback = getRemote(ledger, rollbackTarget)

	// 2f) single-entry ROLLBACK in the lean manifest: flip exactly one entry back.
	const publishedKinds = deployResult.manifest.kinds ?? {}
	const deployedManifest = buildManifest(
		Object.entries(deployResult.manifest.modules).map(([id, dep]) => ({
			moduleId: id,
			version: id === rollbackTarget ? '2.0.0' : dep.version,
			url:
				id === rollbackTarget
					? `https://cdn.example/${id}/2.0.0/remoteEntry.js`
					: dep.url,
			kind: publishedKinds[id] ?? 'route',
		})),
		{ app: HOST_NAME }
	)
	const rolledBack = rollback(deployedManifest, rollbackTarget, version)

	// Every OTHER module must be byte-for-byte untouched by the single-entry flip.
	const untouched = moduleIds
		.filter((id) => id !== rollbackTarget)
		.every(
			(id) =>
				JSON.stringify(requireModule(rolledBack, id)) ===
				JSON.stringify(requireModule(deployedManifest, id))
		)

	// 2g) the enhanced RUNTIME plugin resolves the rolled-back entry at load.
	const mfPlugin = createTreatyMfRuntimePlugin(rolledBack)()
	const resolved = await mfPlugin.resolveRemote({
		remote: { name: rollbackTarget, entry: 'https://STALE/remoteEntry.js' },
	})

	// And the operational deployment-manifest runtime plugin resolves from the ledger.
	const deploymentPlugin = createTreatyDeploymentRuntimePlugin(ledger)()
	const ledgerResolved = await deploymentPlugin.resolveRemote({
		remote: { name: rollbackTarget, entry: 'https://STALE/remoteEntry.js' },
	})

	return {
		host: HOST_NAME,
		authoring: features,
		federation: {
			moduleIds,
			modules,
			viteRemotesCount: Object.keys(viteFederation.exposes).length,
			viteName: viteFederation.name,
		},
		manifest,
		deploy: {
			partial: deployResult.partial,
			uploadedFiles,
			publishedManifest: deployResult.manifest,
		},
		rollback: {
			target: rollbackTarget,
			beforeRollback,
			rolledBackEntry: getModule(rolledBack, rollbackTarget),
			untouched,
			runtimeResolvedEntry: resolved.entry,
			runtimeResolvedVersion: resolved.version,
			ledgerResolvedEntry: ledgerResolved.entry,
			ledgerResolvedVersion: ledgerResolved.version,
		},
	}
}
