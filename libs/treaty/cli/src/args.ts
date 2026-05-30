/**
 * @module
 *
 * A tiny, dependency-free argument parser for the Treaty CLI. It is intentionally
 * minimal — just enough to split `treaty <command> [target] [--flags]` into a
 * structured shape the command handlers consume. We avoid a heavyweight parser
 * dependency because the CLI's surface is small and stable.
 *
 * Grammar handled:
 *   - `--flag` / `--no-flag`         → boolean true / false
 *   - `--key value` / `--key=value`  → string
 *   - `-p 4200`                      → short option (treated like `--p`)
 *   - bare tokens                    → positional arguments
 *   - everything after `--`          → passthrough positionals (verbatim)
 *
 * No coercion beyond booleans is applied here; command handlers interpret the
 * string values they care about (e.g. parsing a port to a number).
 */

/** The structured result of parsing an `argv` tail (without `node`/script). */
export interface ParsedArgs {
	/** The first positional token, e.g. `dev` / `build` / `generate`. */
	readonly command: string | undefined
	/** Remaining positionals after the command, e.g. the generate kind + name. */
	readonly positionals: readonly string[]
	/** Parsed options. Booleans for valueless flags, strings otherwise. */
	readonly options: Readonly<Record<string, string | boolean>>
	/** Raw tokens that appeared after a literal `--` separator. */
	readonly passthrough: readonly string[]
}

/** Strip a leading `--`/`-` from an option token, returning the bare key. */
function optionName(token: string): string {
	return token.startsWith('--') ? token.slice(2) : token.slice(1)
}

/**
 * Parse a CLI `argv` tail into a {@link ParsedArgs}. The caller passes
 * `process.argv.slice(2)` (or any token list). Parsing never throws — malformed
 * input simply yields the best-effort structured shape, leaving validation to
 * the command handlers so error messages can be command-specific.
 */
export function parseArgs(argv: readonly string[]): ParsedArgs {
	const positionals: string[] = []
	const options: Record<string, string | boolean> = {}
	const passthrough: string[] = []

	let i = 0
	for (; i < argv.length; i++) {
		const token = argv[i]!

		// Everything after a bare `--` is passthrough, taken verbatim.
		if (token === '--') {
			for (let j = i + 1; j < argv.length; j++) passthrough.push(argv[j]!)
			break
		}

		if (token.startsWith('-') && token.length > 1) {
			// `--key=value` form: split on the first `=`.
			const eq = token.indexOf('=')
			if (eq !== -1) {
				const key = optionName(token.slice(0, eq))
				options[key] = token.slice(eq + 1)
				continue
			}

			const raw = optionName(token)
			// `--no-foo` negates a boolean flag.
			if (raw.startsWith('no-')) {
				options[raw.slice(3)] = false
				continue
			}

			// A following non-option token is this flag's value; otherwise boolean.
			const next = argv[i + 1]
			if (next !== undefined && !(next.startsWith('-') && next.length > 1) && next !== '--') {
				options[raw] = next
				i++
			} else {
				options[raw] = true
			}
			continue
		}

		positionals.push(token)
	}

	const [command, ...rest] = positionals
	return { command, positionals: rest, options, passthrough }
}

/** Read an option as a string, returning `undefined` for a boolean/absent flag. */
export function stringOption(
	options: Readonly<Record<string, string | boolean>>,
	...keys: readonly string[]
): string | undefined {
	for (const key of keys) {
		const value = options[key]
		if (typeof value === 'string') return value
	}
	return undefined
}

/** Read an option as a boolean. Absent ⇒ `undefined`; a string value ⇒ `true`. */
export function boolOption(
	options: Readonly<Record<string, string | boolean>>,
	...keys: readonly string[]
): boolean | undefined {
	for (const key of keys) {
		const value = options[key]
		if (typeof value === 'boolean') return value
		if (typeof value === 'string') return true
	}
	return undefined
}

/** Read an option as an integer (e.g. a port). Returns `undefined` if unparseable. */
export function numberOption(
	options: Readonly<Record<string, string | boolean>>,
	...keys: readonly string[]
): number | undefined {
	const raw = stringOption(options, ...keys)
	if (raw === undefined) return undefined
	const n = Number.parseInt(raw, 10)
	return Number.isNaN(n) ? undefined : n
}
