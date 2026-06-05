#!/usr/bin/env node
/**
 * @module
 *
 * The `treaty` bin entry. A thin shebang wrapper around {@link run}: it parses
 * `process.argv`, runs the requested command, prints the result to the right
 * stream, and sets the process exit code. All real logic lives in `index.ts` so
 * it stays unit-testable without spawning a process.
 */

import { run } from './index.js'

async function main(): Promise<void> {
	const result = await run(process.argv.slice(2))
	const stream = result.isError ? process.stderr : process.stdout
	stream.write(result.output.join('\n') + '\n')
	process.exitCode = result.exitCode
}

void main().catch((err: unknown) => {
	// A defensive last-resort handler: `run` already converts handled command
	// failures into a non-zero RunResult, so reaching here means an unexpected
	// throw (e.g. a rejected lazy import). Surface it and fail.
	process.stderr.write(`treaty: ${err instanceof Error ? err.stack ?? err.message : String(err)}\n`)
	process.exitCode = 1
})
