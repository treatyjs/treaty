/**
 * @module
 *
 * `@treaty/cli` — the standalone Treaty CLI (`bin: treaty`). It is the driver for
 * Treaty projects that have **no `angular.json`**: a convention-based, zero-config
 * compiler runner. It is not an `ng` wrapper. Three commands:
 *
 *   - `treaty dev`                     — start the Vite/Rspack dev server with the
 *                                        Treaty plugin and automatic Module Federation.
 *   - `treaty build`                   — production build → deployable output.
 *   - `treaty generate <kind> <name>`  — scaffold a standalone, federation-ready
 *                                        app / lib / component.
 *
 * This module is the programmatic entry: `run(argv)` parses args, resolves the
 * convention/`treaty.config` config, and dispatches to a command. The bin file
 * (`cli.ts`) is a thin shebang wrapper around {@link run}. Bundler peers are
 * loaded lazily inside the command handlers, so importing `@treaty/cli` (and the
 * dry-run/help paths) never requires `vite`/`@rspack/*` to be installed.
 */

import {
	parseArgs,
	stringOption,
	boolOption,
	numberOption,
	type ParsedArgs,
} from './args.js'
import {
	resolveConfig,
	type Bundler,
	type ConfigOverrides,
	type ResolvedConfig,
} from './config.js'
import { runDev } from './commands/dev.js'
import { runBuild } from './commands/build.js'
import {
	runGenerate,
	isGenerateKind,
	type GenerateResult,
} from './commands/generate.js'
import { buildViteConfig, buildRspackConfig } from './engine.js'

/** The CLI's exit outcome. `0` = success; non-zero = a handled failure. */
export interface RunResult {
	/** Process exit code the bin wrapper should use. */
	readonly exitCode: number
	/** Human-readable lines the bin wrapper prints (stdout for ok, stderr for errors). */
	readonly output: readonly string[]
	/** `true` when {@link output} should go to stderr. */
	readonly isError: boolean
}

/** The CLI version, surfaced by `--version`. Kept in sync with package.json. */
export const VERSION = '0.0.1'

/** Top-level `--help` text. */
function helpText(): string[] {
	return [
		'treaty — the standalone Treaty CLI (no angular.json required)',
		'',
		'Usage: treaty <command> [options]',
		'',
		'Commands:',
		'  dev                     Start the dev server (Treaty plugin + auto Module Federation)',
		'  build                   Production build to a deployable output directory',
		'  generate <kind> <name>  Scaffold a standalone, federation-ready app | lib | component',
		'',
		'Common options:',
		'  --bundler <vite|rspack> Bundler to drive (default: vite)',
		'  --root <dir>            Project root (default: current directory)',
		'  --config <file>         Explicit treaty.config file (default: auto-discovered)',
		'  --port <n>              Dev-server port (default: 4200)',
		'  --host <h>              Dev-server host (default: localhost)',
		'  --out-dir <dir>         Build output directory (default: dist)',
		'  --base <path>           Public base path (default: /)',
		'  --dry-run               Resolve config/plugins without starting a server or writing files',
		'  --help, -h              Show help',
		'  --version, -v           Show version',
		'',
		'Every Treaty app is a Module Federation host automatically — you configure nothing.',
	]
}

/** Translate parsed CLI flags into {@link ConfigOverrides}. */
function overridesFromArgs(parsed: ParsedArgs): ConfigOverrides {
	const bundlerRaw = stringOption(parsed.options, 'bundler')
	const bundler: Bundler | undefined =
		bundlerRaw === 'vite' || bundlerRaw === 'rspack' ? bundlerRaw : undefined
	return {
		bundler,
		root: stringOption(parsed.options, 'root'),
		outDir: stringOption(parsed.options, 'out-dir', 'outDir'),
		base: stringOption(parsed.options, 'base'),
		host: stringOption(parsed.options, 'host'),
		port: numberOption(parsed.options, 'port', 'p'),
		configFile: stringOption(parsed.options, 'config', 'c'),
	}
}

/** A short summary of the bundler config a dry-run resolved (for diagnostics/tests). */
export interface DryRunSummary {
	readonly bundler: Bundler
	readonly root: string
	readonly outDir: string
	/** The federation container name the auto-MF config resolved to. */
	readonly federationEnabled: boolean
}

/**
 * Resolve (without executing) the bundler config + auto-MF wiring for a command.
 * This is the dry-run core: it builds the real Vite/Rspack config through the
 * engine — exercising `treatyWithFederation` / `TreatyRspackPlugin` and the MF
 * adapters — but never starts a server or writes output. Throws only if the
 * plugin/MF wiring itself fails, which is exactly what the smoke test asserts
 * does *not* happen.
 */
export function dryRunConfig(
	config: ResolvedConfig,
	command: 'dev' | 'build'
): DryRunSummary {
	const federationEnabled = config.moduleFederation !== false
	if (config.bundler === 'rspack') {
		buildRspackConfig(config, command === 'dev' ? 'development' : 'production')
	} else {
		const inline = buildViteConfig(config, command === 'dev' ? 'serve' : 'build')
		// The Vite federation plugin is a lazily-resolved `Promise<Plugin>` that
		// only loads the optional `@module-federation/vite` peer when Vite actually
		// runs. A dry-run never runs Vite, so we attach a no-op `catch` to any
		// pending plugin entries: validating the wiring must not leave a dangling
		// rejection if that optional peer is absent in this environment.
		for (const plugin of inline.plugins ?? []) {
			if (plugin && typeof (plugin as Promise<unknown>).then === 'function') {
				void (plugin as Promise<unknown>).catch(() => undefined)
			}
		}
	}
	return {
		bundler: config.bundler,
		root: config.root,
		outDir: config.outDir,
		federationEnabled,
	}
}

/** Handle `treaty generate <kind> <name>`. */
async function handleGenerate(parsed: ParsedArgs, cwd: string): Promise<RunResult> {
	const [kind, name] = parsed.positionals
	if (!isGenerateKind(kind)) {
		return {
			exitCode: 1,
			isError: true,
			output: [
				`treaty generate: unknown kind ${kind ? `"${kind}"` : '(none)'}.`,
				'Expected one of: app, lib, component.',
				'Usage: treaty generate <app|lib|component> <name>',
			],
		}
	}
	if (!name) {
		return {
			exitCode: 1,
			isError: true,
			output: [`treaty generate ${kind}: a name is required.`, `Usage: treaty generate ${kind} <name>`],
		}
	}

	const dryRun = boolOption(parsed.options, 'dry-run', 'dryRun') ?? false
	const force = boolOption(parsed.options, 'force', 'f') ?? false
	const result: GenerateResult = await runGenerate({ kind, name, cwd, dryRun, force })

	const lines = [`treaty generate ${kind} "${name}"${dryRun ? ' (dry run)' : ''}:`]
	for (const file of result.files) {
		const state = dryRun
			? 'plan'
			: result.written.includes(file.path)
				? 'create'
				: 'skip (exists)'
		lines.push(`  ${state}  ${file.path}`)
	}
	return { exitCode: 0, isError: false, output: lines }
}

/** Handle `treaty dev` / `treaty build`. */
async function handleBundlerCommand(
	command: 'dev' | 'build',
	parsed: ParsedArgs,
	cwd: string
): Promise<RunResult> {
	const config = await resolveConfig(cwd, overridesFromArgs(parsed))
	const dryRun = boolOption(parsed.options, 'dry-run', 'dryRun') ?? false

	if (dryRun) {
		const summary = dryRunConfig(config, command)
		return {
			exitCode: 0,
			isError: false,
			output: [
				`treaty ${command} (dry run): resolved ${summary.bundler} config`,
				`  root:       ${summary.root}`,
				`  outDir:     ${summary.outDir}`,
				`  federation: ${summary.federationEnabled ? 'auto (host)' : 'disabled'}`,
			],
		}
	}

	if (command === 'dev') {
		const server = await runDev(config)
		return {
			exitCode: 0,
			isError: false,
			output: [`treaty dev: ${server.bundler} server running at ${server.url}`],
		}
	}

	const result = await runBuild(config)
	return {
		exitCode: 0,
		isError: false,
		output: [`treaty build: ${result.bundler} build complete → ${result.outDir}`],
	}
}

/**
 * Parse `argv`, resolve config, and run the requested command. Returns a
 * {@link RunResult} rather than calling `process.exit`, so it is fully testable
 * and embeddable; the bin wrapper applies the exit code and prints the output.
 * Errors thrown by command handlers are caught and turned into an error result.
 *
 * @param argv  The arg tail (`process.argv.slice(2)`).
 * @param cwd   The working directory commands resolve paths against.
 */
export async function run(
	argv: readonly string[],
	cwd: string = process.cwd()
): Promise<RunResult> {
	const parsed = parseArgs(argv)

	if (boolOption(parsed.options, 'version', 'v')) {
		return { exitCode: 0, isError: false, output: [VERSION] }
	}
	if (parsed.command === undefined || boolOption(parsed.options, 'help', 'h')) {
		// `--help` with no command, or any `--help`, prints usage with exit 0.
		// A missing command also shows help, but is treated as a usage error.
		const isError = parsed.command === undefined && !boolOption(parsed.options, 'help', 'h')
		return { exitCode: isError ? 1 : 0, isError, output: helpText() }
	}

	try {
		switch (parsed.command) {
			case 'dev':
				return await handleBundlerCommand('dev', parsed, cwd)
			case 'build':
				return await handleBundlerCommand('build', parsed, cwd)
			case 'generate':
			case 'g':
				return await handleGenerate(parsed, cwd)
			default:
				return {
					exitCode: 1,
					isError: true,
					output: [
						`treaty: unknown command "${parsed.command}".`,
						'Run "treaty --help" for usage.',
					],
				}
		}
	} catch (err) {
		const message = err instanceof Error ? err.message : String(err)
		return {
			exitCode: 1,
			isError: true,
			output: [`treaty ${parsed.command}: ${message}`],
		}
	}
}

export { parseArgs, type ParsedArgs } from './args.js'
export {
	resolveConfig,
	findConfigFile,
	loadConfigFile,
	defineConfig,
	DEFAULT_BUNDLER,
	DEFAULT_ENTRY,
	DEFAULT_OUT_DIR,
	DEFAULT_HOST,
	DEFAULT_PORT,
	CONFIG_FILENAMES,
	type TreatyConfig,
	type ResolvedConfig,
	type ConfigOverrides,
	type Bundler,
} from './config.js'
export {
	buildVitePlugins,
	buildViteConfig,
	buildRspackPlugin,
	buildRspackConfig,
	type RunningServer,
} from './engine.js'
export { runDev } from './commands/dev.js'
export { runBuild, type BuildResult } from './commands/build.js'
export {
	runGenerate,
	planGenerate,
	isGenerateKind,
	type GenerateKind,
	type GenerateOptions,
	type GeneratedFile,
	type GenerateResult,
} from './commands/generate.js'
