/**
 * @module
 *
 * The changed-file source: parse a `git diff --name-only` listing, and run git to
 * produce one. CI usually pipes a diff in (so this tool stays pure and testable),
 * but {@link gitChangedFiles} is the convenience that shells `git` when no list is
 * provided.
 */

/**
 * Matches the entry separators git emits: newline, CRLF, or a NUL byte (git's
 * `-z` output). Built from a code point so no literal control character appears
 * in source (keeps `no-control-regex` happy).
 */
const SEPARATOR = new RegExp(`\\r?\\n|${String.fromCharCode(0)}`)

/**
 * Parse the text of a `git diff --name-only` (or any newline/NUL-separated path
 * list) into a clean, de-duplicated, sorted array of forward-slash paths. Blank
 * lines are dropped; backslashes are normalized; surrounding quotes git adds for
 * paths with special characters are stripped.
 */
export function parseChangedFiles(text: string): string[] {
	const seen = new Set<string>()
	for (const rawLine of text.split(SEPARATOR)) {
		let line = rawLine.trim()
		if (line === '') continue
		// git quotes paths with special chars in double quotes; strip a matching pair.
		if (line.length >= 2 && line.startsWith('"') && line.endsWith('"')) {
			line = line.slice(1, -1)
		}
		line = line.replace(/\\/g, '/').trim()
		if (line !== '') seen.add(line)
	}
	return [...seen].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0))
}

/** Options for {@link gitChangedFiles}. */
export interface GitChangedFilesOptions {
	/**
	 * The git revision range / base to diff against, e.g. `'origin/main...HEAD'`
	 * or `'HEAD~1'`. Passed verbatim to `git diff --name-only <base>`. When omitted
	 * the working-tree + staged changes against `HEAD` are used.
	 */
	readonly base?: string
	/** The repo root to run git in. Defaults to the current working directory. */
	readonly cwd?: string
}

/**
 * Shell `git diff --name-only` and return the parsed changed-file list. Imports
 * `node:child_process` lazily so the pure parsing/affected logic carries no
 * runtime dependency on a process spawn (and stays trivially testable). Throws if
 * git is unavailable or the diff fails.
 */
export async function gitChangedFiles(options: GitChangedFilesOptions = {}): Promise<string[]> {
	const { execFileSync } = await import('node:child_process')
	const args = ['diff', '--name-only']
	if (options.base !== undefined && options.base !== '') args.push(options.base)
	const out = execFileSync('git', args, {
		cwd: options.cwd ?? process.cwd(),
		encoding: 'utf8',
		maxBuffer: 64 * 1024 * 1024,
	})
	return parseChangedFiles(out)
}
