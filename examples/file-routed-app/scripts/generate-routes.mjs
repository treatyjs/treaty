// Build-time file-system route generation for the file-routed-app.
//
// This is the JS surface that wires the REAL `treaty_file_routing` engine into
// a bundler build. Treaty's `treaty_file_routing` crate (libs/file-routing) is
// the canonical engine: it scans a `routes/` + `api/` tree and lowers it to
// Angular lazy routes, Module Federation remotes, and a server-endpoint
// manifest. Rather than re-implement that lowering in JS (which drifts), this
// script RUNS the crate's CLI binary (`treaty-file-routing`) over THIS app's
// project root and writes its output to `src/generated/routes.ts`. Nothing here
// is hand-transcribed: every route path, remote, and endpoint in the generated
// file comes straight out of the engine over the app's own routes/ + api/ dirs.
//
// Two CLI runs, one generated module:
//   1. `--emit ts`  -> the Angular `Routes` array of lazy
//      `loadComponent: () => import('../../routes/…')` boundaries plus the
//      `federationRemotes` descriptor array. Emitted verbatim as the head of the
//      generated module. `--style colon` makes dynamic segments Angular-native
//      (`[slug]` -> `:slug`, `[category]/[page]` -> `:category/:page`); the
//      `not-found` file still lowers to the `**` wildcard.
//   2. `--emit json` -> the full GeneratedRouting; we lift its `endpoints`
//      (the api/ manifest, which the TS emit omits) and append them as a typed
//      `apiEndpoints` export so the server-fn / backend layer can consume the
//      same generated artifact.
//
// The default `--import-base` (`../../`) is exactly right for a module living at
// `src/generated/`.
//
// The crate is detached (its own workspace), so it builds independently of the
// rest of the repo. This script `cargo build`s the binary on demand (a no-op
// once compiled) and then runs it; if the cargo toolchain is unavailable it
// fails loudly rather than silently falling back to a stale or hand-written
// table — the whole point is that the generated routes come from the engine.
//
// Run: node scripts/generate-routes.mjs   (also `npm run routes`, and invoked by
// vite.config.ts on buildStart).
import { execFileSync } from 'node:child_process'
import { existsSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join, relative } from 'node:path'

const here = dirname(fileURLToPath(import.meta.url))
const appRoot = join(here, '..')
const repoRoot = join(appRoot, '..', '..')
const crateManifest = join(repoRoot, 'libs', 'file-routing', 'Cargo.toml')
const outFile = join(appRoot, 'src', 'generated', 'routes.ts')

// Dynamic-segment spelling for the generated Angular routes. `colon` is the
// Angular-router-native form (`:slug`), so the generated graph is directly
// consumable by `provideRouter`.
const STYLE = 'colon'

// Path the compiled CLI binary lands at inside the detached crate's target dir.
const binName = process.platform === 'win32' ? 'treaty-file-routing.exe' : 'treaty-file-routing'
const binPath = join(repoRoot, 'libs', 'file-routing', 'target', 'debug', binName)

/** Run a command to completion, throwing on non-zero exit (stderr inherited). */
function run(cmd, args, opts = {}) {
	execFileSync(cmd, args, { stdio: ['ignore', 'inherit', 'inherit'], ...opts })
}

/** Run a command and capture its stdout as a UTF-8 string. */
function capture(cmd, args, opts = {}) {
	return execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'], ...opts })
}

// 1. Ensure the CLI binary is built. `cargo build` is a near-no-op once the
//    crate is compiled, so this is cheap on warm builds and self-healing on
//    cold ones. The crate is detached, hence the explicit --manifest-path.
run('cargo', ['build', '--quiet', '--manifest-path', crateManifest])

if (!existsSync(binPath)) {
	throw new Error(
		`treaty-file-routing binary not found at ${binPath} after cargo build — ` +
			`is the Rust toolchain installed?`,
	)
}

// 2. Run the engine over THIS app's project root. `--emit ts` produces the
//    Angular `Routes` array + federation remotes; `--emit json` carries the
//    api/ endpoint manifest the TS emit omits.
const tsModule = capture(binPath, [appRoot, '--style', STYLE, '--emit', 'ts'])
const generated = JSON.parse(capture(binPath, [appRoot, '--style', STYLE, '--emit', 'json']))

// 3. Append the server-endpoint manifest (lifted verbatim from the engine's
//    JSON) as a typed, ready-to-consume export. This keeps the api/ side of the
//    file-routing convention in the same generated artifact as the routes.
const endpoints = generated.endpoints ?? []
const endpointsExport = `
/**
 * Server endpoints generated from the api/ directory tree by treaty_file_routing.
 * Each handler file maps to one endpoint; dynamic segments render as :param and
 * \`paramNames\` lists them in path order. The backend plugin (axum by default)
 * mounts these handlers — Treaty is a compiler, not a host.
 */
export interface ApiEndpoint {
	/** URL path, leading slash, dynamic segments as :param. */
	readonly path: string
	/** Tree-relative path of the handler file. */
	readonly handlerFile: string
	/** Ordered names of the dynamic parameters in \`path\`. */
	readonly paramNames: readonly string[]
}

export const apiEndpoints: readonly ApiEndpoint[] = ${JSON.stringify(endpoints, null, '\t')} as const
`

// The TS emit ends with a trailing newline after `as const`; concatenate the
// endpoints block onto it so the whole module is one engine-produced artifact.
writeFileSync(outFile, `${tsModule}${endpointsExport}`)

console.log(
	`generate-routes: wrote ${relative(appRoot, outFile)} via treaty-file-routing ` +
		`(${generated.routes.length} top-level route(s), ${generated.remotes.length} remote(s), ` +
		`${endpoints.length} endpoint(s); style=${STYLE})`,
)
