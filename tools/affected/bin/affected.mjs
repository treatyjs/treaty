#!/usr/bin/env node
/**
 * CLI entry for `treaty-affected`. Thin wrapper that runs the built CLI's
 * `main()`. Build the package first (`npm run build` in tools/affected).
 */
import { main } from '../dist/cli.js'

main().catch((err) => {
	process.stderr.write(`${err?.stack ?? err}\n`)
	process.exitCode = 1
})
