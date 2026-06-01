/**
 * @module
 *
 * The CLI CI invokes to get the affected federated-module set. It is intentionally
 * thin over the pure {@link computeAffected} core: parse args, load the graph and
 * changed files from files / stdin / git, run the computation, and print the
 * result as either JSON (default) or a newline list of affected ids (for piping
 * straight into a build/deploy filter).
 *
 * Usage (CI):
 *   treaty-affected --graph graph.json --base origin/main...HEAD
 *   git diff --name-only origin/main | treaty-affected --graph graph.json --files -
 *   treaty-affected --graph graph.json --files changed.txt --format ids
 *
 * Exit codes: 0 on success (whether or not anything is affected), 1 on a usage or
 * graph error. Use `--format ids` + `--fail-on-empty` if CI should treat "nothing
 * affected" as a non-zero signal.
 */

import { affectedFromInput } from './affected.js'
import type { AffectedOptions, AffectedResult } from './affected.js'
import { graphFromFederation } from './federation.js'
import type { FederatedModuleLike } from './federation.js'
import type { ProjectGraphInput } from './graph.js'
import { GraphError } from './graph.js'
import { gitChangedFiles, parseChangedFiles } from './git.js'

/** The shape the CLI prints in `--format json`. */
export interface CliOutput extends AffectedResult {
	/** Count of affected modules — convenient for CI shell checks. */
	readonly count: number
}

/** Parsed CLI options. */
export interface CliOptions {
	graph?: string
	federation?: string
	files?: string
	base?: string
	format: 'json' | 'ids'
	globalTriggers: string[]
	failOnEmpty: boolean
	help: boolean
}

const HELP = `treaty-affected — Nx/Turborepo-style affected at federated-module granularity

Compute the affected federated modules (the changed modules PLUS their transitive
dependents) so CI compiles/tests/deploys only that federation.

Options:
  --graph <file>        Project graph JSON ({ "nodes": [...] }). '-' reads stdin.
  --federation <file>   federatedModules() JSON to seed nodes from (instead of
                        --graph). With --deps it layers dependency edges on.
                        Shape: { "modules": [...], "dependencies": {...},
                                 "extraPaths": {...} }. '-' reads stdin.
  --files <file>        Changed-file list (one path per line). '-' reads stdin.
  --base <rev>          git diff base, e.g. 'origin/main...HEAD'. Used when --files
                        is omitted (shells 'git diff --name-only <base>').
  --global <prefix>     A path whose change affects EVERY module (repeatable).
  --format <json|ids>   Output format. Default: json.
  --fail-on-empty       Exit non-zero when nothing is affected.
  -h, --help            Show this help.
`

/** Parse argv (without node + script) into {@link CliOptions}. Throws on misuse. */
export function parseArgs(argv: readonly string[]): CliOptions {
	const opts: CliOptions = {
		format: 'json',
		globalTriggers: [],
		failOnEmpty: false,
		help: false,
	}
	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i]
		const need = (): string => {
			const v = argv[++i]
			if (v === undefined) throw new CliError(`${arg} requires a value`)
			return v
		}
		switch (arg) {
			case '--graph':
				opts.graph = need()
				break
			case '--federation':
				opts.federation = need()
				break
			case '--files':
				opts.files = need()
				break
			case '--base':
				opts.base = need()
				break
			case '--global':
				opts.globalTriggers.push(need())
				break
			case '--format': {
				const v = need()
				if (v !== 'json' && v !== 'ids') throw new CliError(`invalid --format: ${v}`)
				opts.format = v
				break
			}
			case '--fail-on-empty':
				opts.failOnEmpty = true
				break
			case '-h':
			case '--help':
				opts.help = true
				break
			default:
				throw new CliError(`unknown argument: ${arg}`)
		}
	}
	return opts
}

/** A recoverable CLI usage/IO error (printed to stderr, exit 1). */
export class CliError extends Error {
	override readonly name = 'CliError'
}

/** Read a file path or, when it is `'-'`, read all of stdin. */
async function readSource(path: string, readStdin: () => Promise<string>): Promise<string> {
	if (path === '-') return readStdin()
	const { readFile } = await import('node:fs/promises')
	try {
		return await readFile(path, 'utf8')
	} catch (err) {
		// Surface a missing/unreadable input as a clean CLI error (exit 1) rather
		// than an uncaught throw, so CI gets a readable message.
		throw new CliError(`cannot read ${path}: ${(err as Error).message}`)
	}
}

/** The shape accepted by --federation. */
interface FederationInput {
	readonly modules: readonly FederatedModuleLike[]
	readonly dependencies?: Readonly<Record<string, readonly string[]>>
	readonly extraPaths?: Readonly<Record<string, readonly string[]>>
}

/** Build the {@link ProjectGraphInput} from --graph or --federation. */
async function loadGraphInput(
	opts: CliOptions,
	readStdin: () => Promise<string>
): Promise<ProjectGraphInput> {
	if (opts.graph !== undefined && opts.federation !== undefined) {
		throw new CliError('pass either --graph or --federation, not both')
	}
	if (opts.graph !== undefined) {
		const text = await readSource(opts.graph, readStdin)
		return parseJson<ProjectGraphInput>(text, '--graph')
	}
	if (opts.federation !== undefined) {
		const text = await readSource(opts.federation, readStdin)
		const fed = parseJson<FederationInput>(text, '--federation')
		if (!Array.isArray(fed.modules)) {
			throw new CliError('--federation JSON needs a `modules` array')
		}
		return graphFromFederation(fed.modules, {
			...(fed.dependencies !== undefined ? { dependencies: fed.dependencies } : {}),
			...(fed.extraPaths !== undefined ? { extraPaths: fed.extraPaths } : {}),
		})
	}
	throw new CliError('a project graph is required: pass --graph or --federation')
}

/** Parse JSON with a useful error pointing at which input failed. */
function parseJson<T>(text: string, label: string): T {
	try {
		return JSON.parse(text) as T
	} catch (err) {
		throw new CliError(`${label} is not valid JSON: ${(err as Error).message}`)
	}
}

/** Resolve the changed-file list from --files / --base / git. */
async function loadChangedFiles(
	opts: CliOptions,
	readStdin: () => Promise<string>
): Promise<string[]> {
	if (opts.files !== undefined) {
		const text = await readSource(opts.files, readStdin)
		return parseChangedFiles(text)
	}
	return gitChangedFiles(opts.base !== undefined ? { base: opts.base } : {})
}

/** I/O surface so {@link run} is testable without touching the real process. */
export interface CliIo {
	readonly argv: readonly string[]
	readStdin(): Promise<string>
	stdout(text: string): void
	stderr(text: string): void
}

/**
 * Run the CLI over an injected {@link CliIo} and return the process exit code.
 * Pure of `process` so tests drive it directly; {@link main} wires the real one.
 */
export async function run(io: CliIo): Promise<number> {
	let opts: CliOptions
	try {
		opts = parseArgs(io.argv)
	} catch (err) {
		io.stderr(`${(err as Error).message}\n\n${HELP}`)
		return 1
	}

	if (opts.help) {
		io.stdout(HELP)
		return 0
	}

	let result: AffectedResult
	try {
		const graphInput = await loadGraphInput(opts, io.readStdin)
		const changed = await loadChangedFiles(opts, io.readStdin)
		const affectedOptions: AffectedOptions =
			opts.globalTriggers.length > 0 ? { globalTriggers: opts.globalTriggers } : {}
		result = affectedFromInput(graphInput, changed, affectedOptions)
	} catch (err) {
		if (err instanceof CliError || err instanceof GraphError) {
			io.stderr(`${err.message}\n`)
			return 1
		}
		throw err
	}

	if (opts.format === 'ids') {
		if (result.affectedIds.length > 0) io.stdout(`${result.affectedIds.join('\n')}\n`)
	} else {
		const output: CliOutput = { ...result, count: result.affected.length }
		io.stdout(`${JSON.stringify(output, null, '\t')}\n`)
	}

	if (opts.failOnEmpty && result.affectedIds.length === 0) return 1
	return 0
}

/** Wire {@link run} to the real process (argv, stdin, stdout/stderr, exit). */
export async function main(): Promise<void> {
	const io: CliIo = {
		argv: process.argv.slice(2),
		readStdin: async () => {
			process.stdin.setEncoding('utf8')
			let data = ''
			for await (const chunk of process.stdin) data += chunk
			return data
		},
		stdout: (text) => process.stdout.write(text),
		stderr: (text) => process.stderr.write(text),
	}
	process.exitCode = await run(io)
}
